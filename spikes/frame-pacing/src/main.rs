//! 120Hz 박자 실험. 새 카사라이트 렌더러가 프레임을 어떤 방식으로 낼지 고르려고 잰다
//! (kasaterm `docs/terminal-engine.md` §4.2 의 A/B).
//!
//! 한 번 실행에 네 구간을 돈다: 데우기 → 폭주(매 프레임 200×60 칸 전부 바꿈) → 쉼 → 키(합성 keyDown 을
//! winit 뷰에 직접 넘김). 앱 안 시각(present 반환)과 함께 Metal 이 알려 주는 실제 표시 시각(presentedTime)을
//! 같은 시계(CACurrentMediaTime)로 모은다. 화면 녹화 권한 없이 화면에 실제로 뜬 박자를 보려는 것이다.

#[cfg(target_os = "macos")]
mod mac;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

const COLS: u32 = 200;
const ROWS: u32 = 60;
const CELLS: usize = (COLS * ROWS) as usize;

const WARM: Duration = Duration::from_millis(500);
const FLOOD_END: Duration = Duration::from_millis(3500);
const IDLE_END: Duration = Duration::from_millis(6000);
const KEY_GAP: Duration = Duration::from_millis(150);
const TAIL: Duration = Duration::from_millis(1200);
const IDLE2: Duration = Duration::from_millis(1500);
const REFLOOD: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// 지금 본판: AutoNoVsync + 대기열 1, 애니메이션은 8ms WaitUntil 펌프.
    Timer,
    /// Fifo 로 쉬지 않고 그린다. get_current_texture 가 박자에 막힌다.
    Fifo,
    /// 디스플레이 링크가 박자를 주고 Fifo 로 낸다.
    LinkFifo,
    /// 디스플레이 링크가 박자를 주고 Immediate 로 낸다.
    LinkImm,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "timer" => Mode::Timer,
            "fifo" => Mode::Fifo,
            "link-fifo" => Mode::LinkFifo,
            "link-imm" => Mode::LinkImm,
            _ => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            Mode::Timer => "timer",
            Mode::Fifo => "fifo",
            Mode::LinkFifo => "link-fifo",
            Mode::LinkImm => "link-imm",
        }
    }
    fn linked(self) -> bool {
        matches!(self, Mode::LinkFifo | Mode::LinkImm)
    }
    fn present_mode(self) -> wgpu::PresentMode {
        match self {
            Mode::Timer => wgpu::PresentMode::AutoNoVsync,
            Mode::Fifo | Mode::LinkFifo => wgpu::PresentMode::Fifo,
            Mode::LinkImm => wgpu::PresentMode::Immediate,
        }
    }
}

struct Args {
    mode: Mode,
    keys: usize,
    /// 입력·출력 뒤 링크로 같은 화면을 계속 내는 시간. ProMotion 이 주사율을 내리지 않게 붙잡는다.
    hold: Duration,
    latency: u32,
    /// 붙잡는 동안(링크가 120Hz 로 도는 동안) 키는 바로 그리지 않고 다음 박자에 싣는다. 바로 그리면 박자 그리기와
    /// drawable 을 다툰다.
    defer: bool,
}

fn parse_args() -> Args {
    let mut a = Args { mode: Mode::LinkFifo, keys: 30, hold: Duration::from_millis(1000), latency: 1, defer: false };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--mode" => a.mode = Mode::parse(&v).expect("--mode timer|fifo|link-fifo|link-imm"),
            "--keys" => a.keys = v.parse().expect("--keys N"),
            "--hold-ms" => a.hold = Duration::from_millis(v.parse().expect("--hold-ms N")),
            "--latency" => a.latency = v.parse().expect("--latency N"),
            "--defer" => a.defer = v == "1",
            _ => panic!("unknown arg {k}"),
        }
    }
    a
}

#[derive(Debug, Clone, Copy)]
pub enum User {
    Tick { ts: f64, target: f64 },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Warm,
    Flood,
    Idle,
    Keys,
    /// 키 뒤 다시 쉰다. ProMotion 이 주사율을 내릴 틈이다.
    Idle2,
    /// 쉼 뒤 다시 폭주. 첫 프레임들이 곧바로 120Hz 로 뜨는지 본다.
    Reflood,
    Done,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Inst {
    rgb: u32,
}

const SHADER: &str = r#"
struct VOut { @builtin(position) pos: vec4f, @location(0) col: vec4f };
@vertex fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32, @location(0) c: u32) -> VOut {
  let cell = vec2f(f32(ii % 200u), f32(ii / 200u));
  let corner = vec2f(f32(vi & 1u), f32(vi >> 1u));
  let xy = vec2f(-1.0, 1.0) + (cell + corner * 0.9) * vec2f(2.0 / 200.0, -2.0 / 60.0);
  var o: VOut;
  o.pos = vec4f(xy, 0.0, 1.0);
  o.col = vec4f(f32(c & 255u), f32((c >> 8u) & 255u), f32((c >> 16u) & 255u), 255.0) / 255.0;
  return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4f { return i.col; }
"#;

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    insts: wgpu::Buffer,
}

#[derive(Default)]
struct Rec {
    /// (seq, phase, media time right after present() returned)
    presents: Vec<(u64, Phase, f64)>,
    acquire_ms: Vec<(Phase, f64)>,
    cpu_ms: Vec<(Phase, f64)>,
    ticks: Vec<(Phase, f64, f64)>,
    idle_wakeups: u32,
    idle_renders: u32,
    /// (inject, winit receive, present return, frame seq)
    keys: Vec<(f64, f64, f64, u64)>,
    tick_to_present_ms: Vec<f64>,
}

struct App {
    args: Args,
    proxy: EventLoopProxy<User>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    #[cfg(target_os = "macos")]
    link: Option<mac::Link>,
    cells: Vec<Inst>,
    dirty: bool,
    seq: u64,
    start: Option<Instant>,
    last_render: Instant,
    hold_until: Option<Instant>,
    next_key: usize,
    pending_key: Option<f64>,
    deferred_key: Option<(f64, f64)>,
    rec: Rec,
    screen_max_fps: i64,
    refresh_mhz: u32,
}

impl App {
    fn phase(&self) -> Phase {
        let Some(s) = self.start else { return Phase::Warm };
        let t = s.elapsed();
        if t < WARM {
            Phase::Warm
        } else if t < FLOOD_END {
            Phase::Flood
        } else if t < IDLE_END {
            Phase::Idle
        } else if t < self.keys_end() {
            Phase::Keys
        } else if t < self.keys_end() + IDLE2 {
            Phase::Idle2
        } else if t < self.keys_end() + IDLE2 + REFLOOD {
            Phase::Reflood
        } else {
            Phase::Done
        }
    }

    fn keys_end(&self) -> Duration {
        IDLE_END + KEY_GAP * self.args.keys as u32 + TAIL
    }

    /// 지금 구간이 끝나는 시각(키 구간이면 다음 키 시각).
    fn next_boundary(&self, start: Instant) -> Instant {
        let end = match self.phase() {
            Phase::Warm => WARM,
            Phase::Flood => FLOOD_END,
            Phase::Idle => IDLE_END,
            Phase::Keys if self.next_key < self.args.keys => IDLE_END + KEY_GAP * self.next_key as u32,
            Phase::Keys => self.keys_end(),
            Phase::Idle2 => self.keys_end() + IDLE2,
            Phase::Reflood | Phase::Done => self.keys_end() + IDLE2 + REFLOOD,
        };
        start + end
    }

    fn flooding(&self) -> bool {
        matches!(self.phase(), Phase::Warm | Phase::Flood | Phase::Reflood)
    }

    fn holding(&self) -> bool {
        self.hold_until.is_some_and(|h| Instant::now() < h)
    }

    fn render(&mut self) -> Option<u64> {
        let phase = self.phase();
        let gpu = self.gpu.as_mut()?;
        self.seq += 1;
        #[cfg(target_os = "macos")]
        mac::set_frame_seq(self.seq);
        let t0 = Instant::now();
        let frame = match gpu.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return None;
            }
        };
        let acquire = t0.elapsed().as_secs_f64() * 1e3;
        if self.dirty {
            gpu.queue.write_buffer(&gpu.insts, 0, bytemuck::cast_slice(&self.cells));
            self.dirty = false;
        }
        let view = frame.texture.create_view(&Default::default());
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_vertex_buffer(0, gpu.insts.slice(..));
            pass.draw(0..4, 0..CELLS as u32);
        }
        gpu.queue.submit([enc.finish()]);
        frame.present();
        let cpu = t0.elapsed().as_secs_f64() * 1e3;
        self.last_render = Instant::now();
        if phase != Phase::Warm {
            self.rec.presents.push((self.seq, phase, now_media()));
            self.rec.acquire_ms.push((phase, acquire));
            self.rec.cpu_ms.push((phase, cpu));
        }
        if phase == Phase::Idle {
            self.rec.idle_renders += 1;
        }
        Some(self.seq)
    }

    fn flood_step(&mut self) {
        let k = self.seq as u32;
        for (i, c) in self.cells.iter_mut().enumerate() {
            let v = (i as u32).wrapping_mul(2654435761).wrapping_add(k.wrapping_mul(40503));
            c.rgb = v & 0x7f7f7f;
        }
        self.dirty = true;
    }

    fn key_step(&mut self) {
        let i = (self.next_key * 37) % CELLS;
        self.cells[i].rgb ^= 0xffffff;
        self.dirty = true;
    }

    #[cfg(target_os = "macos")]
    fn set_link_running(&self, on: bool) {
        if let Some(l) = &self.link {
            l.set_paused(!on);
        }
    }

    fn finish(&mut self, el: &ActiveEventLoop) {
        if el.exiting() {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            self.set_link_running(false);
        }
        // presentedTime 손잡이는 표시 뒤에 불린다. 마지막 몇 프레임 몫을 기다린다.
        std::thread::sleep(Duration::from_millis(150));
        #[cfg(target_os = "macos")]
        let glass = mac::take_presented();
        #[cfg(not(target_os = "macos"))]
        let glass: Vec<(u64, f64)> = Vec::new();
        #[cfg(target_os = "macos")]
        eprintln!("nextDrawable ms {}", dist(&mac::take_next_drawable_ms()));
        println!("{}", report(self, &glass));
        if let Some(path) = std::env::var_os("KASALITE_TRACE") {
            write_trace(&self.rec, &glass, std::path::Path::new(&path));
        }
        el.exit();
    }
}

/// bench/README.md 의 KASALITE_TRACE 줄을 쓴다. 실험에서는 끝에 한꺼번에 쓰지만, 앱은 그때그때 덧붙인다.
fn write_trace(r: &Rec, glass: &[(u64, f64)], path: &std::path::Path) {
    use std::io::Write;
    let mut out = String::new();
    let wall = |media: f64| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
        now - (now_media() - media)
    };
    if let Some(p) = r.presents.first() {
        out += &format!("{{\"ev\":\"first_present\",\"wall\":{:.6}}}\n", wall(p.2));
    }
    let flood: Vec<f64> = r.presents.iter().filter(|p| p.1 == Phase::Flood).map(|p| p.2).collect();
    if let (Some(a), Some(b)) = (flood.first(), flood.last()) {
        out += &format!("{{\"ev\":\"flood\",\"t\":{a:.6}}}\n{{\"ev\":\"flood\",\"t\":{b:.6}}}\n");
    }
    for p in &r.presents {
        out += &format!("{{\"ev\":\"present\",\"t\":{:.6},\"seq\":{}}}\n", p.2, p.0);
    }
    for g in glass {
        out += &format!("{{\"ev\":\"shown\",\"t\":{:.6},\"seq\":{}}}\n", g.1, g.0);
    }
    for k in &r.keys {
        out += &format!("{{\"ev\":\"key\",\"t\":{:.6},\"seq\":{}}}\n", k.1, k.3);
    }
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(out.as_bytes()));
}

fn now_media() -> f64 {
    #[cfg(target_os = "macos")]
    {
        mac::media_time()
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::sync::OnceLock;
        static T0: OnceLock<Instant> = OnceLock::new();
        T0.get_or_init(Instant::now).elapsed().as_secs_f64()
    }
}

impl ApplicationHandler<User> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let best = el.available_monitors().max_by_key(|m| m.refresh_rate_millihertz().unwrap_or(0));
        let mut attrs = Window::default_attributes()
            .with_title("frame-pacing")
            .with_inner_size(winit::dpi::LogicalSize::new(640.0, 400.0))
            .with_active(false);
        if let Some(m) = &best {
            self.refresh_mhz = m.refresh_rate_millihertz().unwrap_or(0);
            // 사람이 일하는 자리를 덜 가리게 오른쪽 위 구석에 둔다. 가려지면 표시가 안 돼 잴 수 없다.
            let (p, size, scale) = (m.position(), m.size(), m.scale_factor());
            let w = (640.0 * scale) as i32;
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(p.x + size.width as i32 - w - 40, p.y + 80));
        }
        let window = Arc::new(el.create_window(attrs).expect("window"));
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).expect("device");
        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: self.args.mode.present_mode(),
            desired_maximum_frame_latency: self.args.latency,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 4,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Uint32],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let insts = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (CELLS * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.gpu = Some(Gpu { surface, device, queue, config, pipeline, insts });
        #[cfg(target_os = "macos")]
        {
            mac::install_presented_trace();
            self.screen_max_fps = mac::screen_max_fps(&window);
            if self.args.mode.linked() {
                self.link = Some(mac::Link::new(&window, self.proxy.clone()));
            }
        }
        self.window = Some(window);
        self.start = Some(Instant::now());
        self.dirty = true;
    }

    /// 쉬는 동안 앱이 스스로 깬 횟수: 시간 맞춤·폴링·링크 박자. 사람이 창 위에서 마우스를 움직여 생긴 사건은 빼려고
    /// 원인으로 가른다.
    fn new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        use winit::event::StartCause;
        if self.phase() == Phase::Idle && matches!(cause, StartCause::ResumeTimeReached { .. } | StartCause::Poll) {
            self.rec.idle_wakeups += 1;
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: User) {
        let User::Tick { ts, target } = ev;
        let phase = self.phase();
        if phase == Phase::Idle {
            self.rec.idle_wakeups += 1;
        }
        self.rec.ticks.push((phase, ts, target));
        if self.flooding() {
            self.flood_step();
        }
        if self.flooding() || self.holding() || self.dirty {
            if let Some(seq) = self.render() {
                self.rec.tick_to_present_ms.push((now_media() - ts) * 1e3);
                if let Some((inject, recv)) = self.deferred_key.take() {
                    self.rec.keys.push((inject, recv, now_media(), seq));
                }
            }
        } else {
            #[cfg(target_os = "macos")]
            self.set_link_running(false);
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let recv = now_media();
                self.key_step();
                if self.args.defer && self.holding() {
                    if let Some(inject) = self.pending_key.take() {
                        self.deferred_key = Some((inject, recv));
                    }
                    self.hold_until = Some(Instant::now() + self.args.hold);
                    return;
                }
                let seq = self.render();
                let ret = now_media();
                if let (Some(inject), Some(seq)) = (self.pending_key.take(), seq) {
                    self.rec.keys.push((inject, recv, ret, seq));
                }
                if self.args.mode.linked() && !self.args.hold.is_zero() {
                    self.hold_until = Some(Instant::now() + self.args.hold);
                    #[cfg(target_os = "macos")]
                    self.set_link_running(true);
                }
            }
            WindowEvent::Resized(s) => {
                if let Some(g) = self.gpu.as_mut() {
                    g.config.width = s.width.max(1);
                    g.config.height = s.height.max(1);
                    g.surface.configure(&g.device, &g.config);
                }
            }
            WindowEvent::RedrawRequested => {}
            WindowEvent::CloseRequested => std::process::exit(0),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_none() {
            return;
        }
        let phase = self.phase();
        if phase == Phase::Done {
            self.finish(el);
            return;
        }
        let start = self.start.unwrap();
        if phase == Phase::Keys {
            let due = start + IDLE_END + KEY_GAP * self.next_key as u32;
            if self.next_key < self.args.keys && Instant::now() >= due {
                #[cfg(target_os = "macos")]
                if let Some(w) = &self.window {
                    self.pending_key = Some(now_media());
                    mac::inject_key(w);
                }
                self.next_key += 1;
            }
        }
        let wake = self.next_boundary(start);
        match self.args.mode {
            Mode::Timer if self.flooding() => {
                let due = self.last_render + Duration::from_millis(8);
                if Instant::now() >= due {
                    self.flood_step();
                    self.render();
                }
                el.set_control_flow(ControlFlow::WaitUntil((self.last_render + Duration::from_millis(8)).min(wake)));
            }
            Mode::Fifo if self.flooding() => {
                self.flood_step();
                self.render();
                el.set_control_flow(ControlFlow::Poll);
            }
            m => {
                #[cfg(target_os = "macos")]
                if m.linked() && self.flooding() {
                    self.set_link_running(true);
                }
                let _ = m;
                el.set_control_flow(ControlFlow::WaitUntil(wake));
            }
        }
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn dist(v: &[f64]) -> String {
    let mut v = v.to_vec();
    let n = v.len();
    format!(
        "{{\"n\":{n},\"p50\":{:.2},\"p95\":{:.2},\"p99\":{:.2},\"max\":{:.2}}}",
        pct(&mut v, 0.5),
        pct(&mut v, 0.95),
        pct(&mut v, 0.99),
        pct(&mut v, 1.0)
    )
}

fn intervals(ts: &[f64]) -> Vec<f64> {
    ts.windows(2).map(|w| (w[1] - w[0]) * 1e3).filter(|d| *d > 0.0).collect()
}

fn report(app: &App, glass: &[(u64, f64)]) -> String {
    use std::collections::HashMap;
    let r = &app.rec;
    let phase_of: HashMap<u64, Phase> = r.presents.iter().map(|p| (p.0, p.1)).collect();
    let shown = |ph: Phase| -> (Vec<f64>, usize) {
        let mut v: Vec<f64> =
            glass.iter().filter(|g| phase_of.get(&g.0) == Some(&ph) && g.1 > 0.0).map(|g| g.1).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let dropped = glass.iter().filter(|g| phase_of.get(&g.0) == Some(&ph) && g.1 == 0.0).count();
        (v, dropped)
    };
    let fps = |v: &[f64]| match (v.first(), v.last()) {
        (Some(a), Some(b)) if b > a => (v.len() - 1) as f64 / (b - a),
        _ => 0.0,
    };
    let of = |v: &[(Phase, f64)], ph: Phase| -> Vec<f64> { v.iter().filter(|c| c.0 == ph).map(|c| c.1).collect() };
    let (flood, flood_drop) = shown(Phase::Flood);
    let (keys_shown, _) = shown(Phase::Keys);
    let (reflood, reflood_drop) = shown(Phase::Reflood);
    let reflood_iv = intervals(&reflood);
    let first: Vec<String> = reflood_iv.iter().take(12).map(|d| format!("{d:.1}")).collect();
    let flood_present: Vec<f64> = r.presents.iter().filter(|p| p.1 == Phase::Flood).map(|p| p.2).collect();
    let flood_ticks: Vec<f64> = r.ticks.iter().filter(|t| t.0 == Phase::Flood).map(|t| t.1).collect();
    let glass_of = |seq: u64| glass.iter().find(|g| g.0 == seq).map(|g| g.1).filter(|t| *t > 0.0);
    let key_present: Vec<f64> = r.keys.iter().map(|k| (k.2 - k.1) * 1e3).collect();
    let key_glass: Vec<f64> = r.keys.iter().filter_map(|k| glass_of(k.3).map(|g| (g - k.1) * 1e3)).collect();
    let key_inject: Vec<f64> = r.keys.iter().map(|k| (k.1 - k.0) * 1e3).collect();
    format!(
        concat!(
            "{{\"mode\":\"{}\",\"latency\":{},\"hold_ms\":{},\"monitor_mhz\":{},\"screen_max_fps\":{},",
            "\"flood\":{{\"glass_fps\":{:.1},\"glass_interval_ms\":{},\"dropped\":{},\"present_interval_ms\":{},",
            "\"tick_interval_ms\":{},\"cpu_ms\":{},\"acquire_ms\":{}}},",
            "\"idle\":{{\"wakeups\":{},\"renders\":{}}},",
            "\"keys\":{{\"n\":{},\"event_to_present_ms\":{},\"event_to_glass_ms\":{},\"inject_to_event_ms\":{},\"glass_interval_ms\":{}}},",
            "\"reflood\":{{\"glass_fps\":{:.1},\"glass_interval_ms\":{},\"dropped\":{},\"first_intervals_ms\":[{}]}},",
            "\"tick_to_present_ms\":{}}}"
        ),
        app.args.mode.name(),
        app.args.latency,
        app.args.hold.as_millis(),
        app.refresh_mhz,
        app.screen_max_fps,
        fps(&flood),
        dist(&intervals(&flood)),
        flood_drop,
        dist(&intervals(&flood_present)),
        dist(&intervals(&flood_ticks)),
        dist(&of(&r.cpu_ms, Phase::Flood)),
        dist(&of(&r.acquire_ms, Phase::Flood)),
        r.idle_wakeups,
        r.idle_renders,
        r.keys.len(),
        dist(&key_present),
        dist(&key_glass),
        dist(&key_inject),
        dist(&intervals(&keys_shown)),
        fps(&reflood),
        dist(&reflood_iv),
        reflood_drop,
        first.join(","),
        dist(&r.tick_to_present_ms),
    )
}

fn main() {
    let args = parse_args();
    let mut builder = EventLoop::<User>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory).with_activate_ignoring_other_apps(false);
    }
    let el = builder.build().expect("event loop");
    let proxy = el.create_proxy();
    let mut app = App {
        args,
        proxy,
        window: None,
        gpu: None,
        #[cfg(target_os = "macos")]
        link: None,
        cells: vec![Inst { rgb: 0 }; CELLS],
        dirty: false,
        seq: 0,
        start: None,
        last_render: Instant::now(),
        hold_until: None,
        next_key: 0,
        pending_key: None,
        deferred_key: None,
        rec: Rec::default(),
        screen_max_fps: 0,
        refresh_mhz: 0,
    };
    el.run_app(&mut app).expect("run");
}
