//! 앱 안 계측 — kasalite `bench/README.md` 의 계약. `KASALITE_TRACE` 면 표시 기록을, `KASALITE_BENCH` 면
//! 출력 폭주와 키 입력을 스스로 일으킨다. `KASALITE_AUTOSEND` 는 초점 칸에 글 한 줄을 넣고,
//! `KASALITE_CAPTURE_PATH`·`_MS` 는 그 시각의 장을 PNG 로 굽고, `KASALITE_QUIT_MS` 는 스스로 닫는다.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};

pub fn media_time() -> f64 {
    #[cfg(target_os = "macos")]
    {
        crate::mac::media_time()
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::sync::OnceLock;
        static T0: OnceLock<Instant> = OnceLock::new();
        T0.get_or_init(Instant::now).elapsed().as_secs_f64()
    }
}

pub struct Trace {
    out: BufWriter<File>,
    first: bool,
}

impl Trace {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("KASALITE_TRACE")?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        #[cfg(target_os = "macos")]
        crate::mac::install_presented_trace();
        Some(Self { out: BufWriter::new(file), first: true })
    }

    /// `cpu` 는 그 장을 짓고 낸 데 든 시간(ms) — 박자를 놓친 까닭이 그리기인지 가른다.
    pub fn present(&mut self, seq: u64, cpu: f64) {
        let t = media_time();
        if self.first {
            self.first = false;
            let wall = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let _ = writeln!(self.out, r#"{{"ev":"first_present","wall":{wall:.6}}}"#);
        }
        let _ = writeln!(self.out, r#"{{"ev":"present","t":{t:.6},"seq":{seq},"cpu":{cpu:.3}}}"#);
        #[cfg(target_os = "macos")]
        for (s, at) in crate::mac::take_presented() {
            let _ = writeln!(self.out, r#"{{"ev":"shown","t":{at:.6},"seq":{s}}}"#);
        }
    }

    pub fn key(&mut self, t: f64, seq: u64) {
        let _ = writeln!(self.out, r#"{{"ev":"key","t":{t:.6},"seq":{seq}}}"#);
    }

    pub fn flood(&mut self) {
        let _ = writeln!(self.out, r#"{{"ev":"flood","t":{:.6}}}"#, media_time());
    }

    pub fn flush(&mut self) {
        #[cfg(target_os = "macos")]
        for (s, at) in crate::mac::take_presented() {
            let _ = writeln!(self.out, r#"{{"ev":"shown","t":{at:.6},"seq":{s}}}"#);
        }
        let _ = self.out.flush();
    }
}

/// 벤치가 이번에 할 일.
pub enum Step {
    /// 초점 칸에 글을 보낸다.
    Send(Vec<u8>),
    FloodStart,
    FloodEnd,
    Key,
    /// 다음 장을 PNG 로 굽는다(화면 녹화 권한 없이 검증).
    Capture(String),
    Quit,
}

pub struct Bench {
    start: Instant,
    plan: Vec<(Duration, Step)>,
}

impl Bench {
    pub fn from_env() -> Option<Self> {
        let mut steps: Vec<(Duration, Step)> = Vec::new();
        if let Ok(text) = std::env::var("KASALITE_AUTOSEND") {
            let ms = std::env::var("KASALITE_AUTOSEND_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(2000);
            let mut bytes = text.into_bytes();
            bytes.push(b'\r');
            steps.push((Duration::from_millis(ms), Step::Send(bytes)));
        }
        if let Ok(path) = std::env::var("KASALITE_CAPTURE_PATH") {
            let ms = std::env::var("KASALITE_CAPTURE_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(4000);
            steps.push((Duration::from_millis(ms), Step::Capture(path)));
        }
        if let Some(ms) = std::env::var("KASALITE_QUIT_MS").ok().and_then(|s| s.parse().ok()) {
            steps.push((Duration::from_millis(ms), Step::Quit));
        }
        if let Ok(spec) = std::env::var("KASALITE_BENCH") {
            let mut keys = 40u64;
            let mut flood_ms = 3000u64;
            for kv in spec.split(',') {
                match kv.split_once('=') {
                    Some(("keys", v)) => keys = v.parse().unwrap_or(keys),
                    Some(("flood_ms", v)) => flood_ms = v.parse().unwrap_or(flood_ms),
                    _ => {}
                }
            }
            // 색 섞인 같은 줄을 `yes` 로 흘린다 — 셸 루프보다 훨씬 빨라 PTY 가 쉬지 않는다.
            let flood = "yes \"$(printf '\\033[31mkasalite \\033[32mflood \\033[33m0123456789 \\033[34mabcdefghijklmnopqrstuvwxyz \\033[35m가나다라마바사\\033[0m')\"\r";
            let t0 = Duration::from_millis(1500);
            steps.push((t0, Step::Send(flood.as_bytes().to_vec())));
            steps.push((t0 + Duration::from_millis(100), Step::FloodStart));
            let t1 = t0 + Duration::from_millis(100 + flood_ms);
            steps.push((t1, Step::FloodEnd));
            steps.push((t1, Step::Send(b"\x03".to_vec())));
            steps.push((t1 + Duration::from_millis(200), Step::Send(b"clear\r".to_vec())));
            let k0 = t1 + Duration::from_millis(2000);
            // 150ms 는 120Hz 한 박자(8.33ms)의 꼭 18배라, 고른 간격이면 키가 늘 박자의 같은 자리에 떨어진다.
            // 사람 손처럼 박자와 무관하게 0~8ms 를 흩뜨린다.
            for i in 0..keys {
                let jitter = (i * 37 % 83) * 100;
                steps.push((k0 + Duration::from_millis(150 * i) + Duration::from_micros(jitter), Step::Key));
            }
            steps.push((k0 + Duration::from_millis(150 * keys + 1200), Step::Quit));
        }
        if steps.is_empty() {
            return None;
        }
        steps.sort_by_key(|(d, _)| *d);
        Some(Self { start: Instant::now(), plan: steps })
    }

    /// 지금까지 때가 된 일들.
    pub fn due(&mut self) -> Vec<Step> {
        let now = self.start.elapsed();
        let n = self.plan.iter().take_while(|(d, _)| *d <= now).count();
        self.plan.drain(..n).map(|(_, s)| s).collect()
    }

    pub fn next_at(&self) -> Option<Instant> {
        self.plan.first().map(|(d, _)| self.start + *d)
    }
}
