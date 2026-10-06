//! macOS 쪽: 디스플레이 링크, 실제 표시 시각 계측, 합성 키.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSView};
use objc2_foundation::{ns_string, NSObject, NSPoint, NSRunLoop, NSRunLoopCommonModes};
use objc2_quartz_core::{CADisplayLink, CAFrameRateRange};
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::User;

pub fn media_time() -> f64 {
    objc2_quartz_core::CACurrentMediaTime()
}

fn ns_view(window: &Window) -> &NSView {
    let RawWindowHandle::AppKit(h) = window.window_handle().expect("handle").as_raw() else { panic!("not appkit") };
    unsafe { h.ns_view.cast::<NSView>().as_ref() }
}

pub fn screen_max_fps(window: &Window) -> i64 {
    let view = ns_view(window);
    view.window().and_then(|w| w.screen()).map(|s| s.maximumFramesPerSecond() as i64).unwrap_or(0)
}

// ---- 실제 표시 시각 ----------------------------------------------------------------------------
//
// wgpu 는 drawable 을 밖에 내놓지 않는다. 그래서 `-[CAMetalLayer nextDrawable]` 을 이 프로세스 안에서만
// 감싸, wgpu 가 받는 drawable 마다 presented 손잡이를 단다. 손잡이는 그 프레임이 화면에 뜬 시각을
// CACurrentMediaTime 시계로 준다(못 뜨고 버려졌으면 0).

static FRAME_SEQ: AtomicU64 = AtomicU64::new(0);
static PRESENTED: Mutex<Vec<(u64, f64)>> = Mutex::new(Vec::new());
static ORIGINAL: Mutex<Option<Imp>> = Mutex::new(None);
static NEXT_DRAWABLE_MS: Mutex<Vec<f64>> = Mutex::new(Vec::new());

pub fn take_next_drawable_ms() -> Vec<f64> {
    std::mem::take(&mut *NEXT_DRAWABLE_MS.lock().unwrap())
}

pub fn set_frame_seq(seq: u64) {
    FRAME_SEQ.store(seq, Ordering::Relaxed);
}

pub fn take_presented() -> Vec<(u64, f64)> {
    std::mem::take(&mut *PRESENTED.lock().unwrap())
}

unsafe extern "C-unwind" fn next_drawable_traced(this: *mut AnyObject, sel: Sel) -> *mut AnyObject {
    let imp = ORIGINAL.lock().unwrap().expect("original nextDrawable");
    let orig: unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject = unsafe { std::mem::transmute(imp) };
    let t0 = media_time();
    let drawable = unsafe { orig(this, sel) };
    NEXT_DRAWABLE_MS.lock().unwrap().push((media_time() - t0) * 1e3);
    if !drawable.is_null() {
        let seq = FRAME_SEQ.load(Ordering::Relaxed);
        let block = RcBlock::new(move |d: *mut AnyObject| {
            let t: f64 = unsafe { msg_send![d, presentedTime] };
            PRESENTED.lock().unwrap().push((seq, t));
        });
        let _: () = unsafe { msg_send![drawable, addPresentedHandler: &*block] };
    }
    drawable
}

pub fn install_presented_trace() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let class = AnyClass::get(c"CAMetalLayer").expect("CAMetalLayer");
        let method = class.instance_method(sel!(nextDrawable)).expect("nextDrawable");
        let traced: Imp = unsafe { std::mem::transmute(next_drawable_traced as unsafe extern "C-unwind" fn(_, _) -> _) };
        let old = unsafe { method.set_implementation(traced) };
        *ORIGINAL.lock().unwrap() = Some(old);
    });
}

// ---- 디스플레이 링크 ---------------------------------------------------------------------------

pub struct LinkIvars {
    proxy: EventLoopProxy<User>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KasaFramePacingLinkTarget"]
    #[ivars = LinkIvars]
    struct LinkTarget;

    impl LinkTarget {
        #[unsafe(method(tick:))]
        fn tick(&self, link: &CADisplayLink) {
            let _ = self.ivars().proxy.send_event(User::Tick { ts: link.timestamp(), target: link.targetTimestamp() });
        }
    }
);

pub struct Link {
    link: Retained<CADisplayLink>,
    _target: Retained<LinkTarget>,
}

impl Link {
    /// macOS 14+ `-[NSView displayLinkWithTarget:selector:]`. 창이 옮겨 간 화면의 주사율을 저절로 따른다.
    pub fn new(window: &Window, proxy: EventLoopProxy<User>) -> Self {
        let mtm = MainThreadMarker::new().expect("main thread");
        let view = ns_view(window);
        let target = LinkTarget::alloc(mtm).set_ivars(LinkIvars { proxy });
        let target: Retained<LinkTarget> = unsafe { msg_send![super(target), init] };
        let link = unsafe { view.displayLinkWithTarget_selector(&target, sel!(tick:)) };
        link.setPreferredFrameRateRange(CAFrameRateRange { minimum: 80.0, maximum: 120.0, preferred: 120.0 });
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        link.setPaused(true);
        Link { link, _target: target }
    }

    pub fn set_paused(&self, paused: bool) {
        if self.link.isPaused() != paused {
            self.link.setPaused(paused);
        }
    }
}

// ---- 합성 키 -----------------------------------------------------------------------------------

/// 사람 손에서 오는 것과 같은 keyDown/keyUp 을 winit 뷰에 바로 넘긴다. 창 서버를 거치지 않아
/// 사람 화면의 초점은 그대로다.
pub fn inject_key(window: &Window) {
    let view = ns_view(window);
    let Some(ns_window) = view.window() else { return };
    let number = ns_window.windowNumber();
    for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let ev = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(),
            media_time(),
            number,
            None,
            ns_string!("a"),
            ns_string!("a"),
            false,
            0,
        );
        let Some(ev) = ev else { continue };
        unsafe {
            if kind == NSEventType::KeyDown {
                let _: () = msg_send![view, keyDown: &*ev];
            } else {
                let _: () = msg_send![view, keyUp: &*ev];
            }
        }
    }
}

