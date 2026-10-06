//! macOS: 디스플레이 링크 박자, 실제 표시 시각(presentedTime) 계측, 합성 키.
//! 박자 방식은 spikes/frame-pacing 실측으로 골랐다 — 링크 + Immediate + drawable 3장.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSView};
use objc2_foundation::{NSObject, NSPoint, NSRunLoop, NSRunLoopCommonModes, NSString};
use objc2_quartz_core::{CADisplayLink, CAFrameRateRange};
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::app::UserEvent;

pub fn media_time() -> f64 {
    objc2_quartz_core::CACurrentMediaTime()
}

fn ns_view(window: &Window) -> Option<&NSView> {
    let RawWindowHandle::AppKit(h) = window.window_handle().ok()?.as_raw() else { return None };
    Some(unsafe { h.ns_view.cast::<NSView>().as_ref() })
}

// ---- 디스플레이 링크 ---------------------------------------------------------------------------

pub struct LinkIvars {
    proxy: EventLoopProxy<UserEvent>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KasaLiteLinkTarget"]
    #[ivars = LinkIvars]
    struct LinkTarget;

    impl LinkTarget {
        #[unsafe(method(tick:))]
        fn tick(&self, _link: &CADisplayLink) {
            let _ = self.ivars().proxy.send_event(UserEvent::Tick);
        }
    }
);

pub struct Link {
    link: Retained<CADisplayLink>,
    _target: Retained<LinkTarget>,
}

impl Link {
    /// macOS 14+ `-[NSView displayLinkWithTarget:selector:]` — 창이 옮겨 간 화면의 주사율을 저절로 따른다.
    pub fn new(window: &Window, proxy: EventLoopProxy<UserEvent>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let view = ns_view(window)?;
        let target = LinkTarget::alloc(mtm).set_ivars(LinkIvars { proxy });
        let target: Retained<LinkTarget> = unsafe { msg_send![super(target), init] };
        let link = unsafe { view.displayLinkWithTarget_selector(&target, sel!(tick:)) };
        link.setPreferredFrameRateRange(CAFrameRateRange { minimum: 80.0, maximum: 120.0, preferred: 120.0 });
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        link.setPaused(true);
        Some(Link { link, _target: target })
    }

    pub fn set_running(&self, on: bool) {
        if self.link.isPaused() == on {
            self.link.setPaused(!on);
        }
    }
}

// ---- 실제 표시 시각 ----------------------------------------------------------------------------
//
// wgpu 는 drawable 을 밖에 내놓지 않는다. `-[CAMetalLayer nextDrawable]` 을 이 프로세스 안에서만 감싸
// drawable 마다 presented 손잡이를 단다. 시각은 CACurrentMediaTime 시계(버려진 장은 0).

static FRAME_SEQ: AtomicU64 = AtomicU64::new(0);
static PRESENTED: Mutex<Vec<(u64, f64)>> = Mutex::new(Vec::new());
static ORIGINAL: Mutex<Option<Imp>> = Mutex::new(None);

pub fn set_frame_seq(seq: u64) {
    FRAME_SEQ.store(seq, Ordering::Relaxed);
}

pub fn take_presented() -> Vec<(u64, f64)> {
    std::mem::take(&mut *PRESENTED.lock().unwrap())
}

unsafe extern "C-unwind" fn next_drawable_traced(this: *mut AnyObject, sel: Sel) -> *mut AnyObject {
    let imp = ORIGINAL.lock().unwrap().expect("original nextDrawable");
    let orig: unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject = unsafe { std::mem::transmute(imp) };
    let drawable = unsafe { orig(this, sel) };
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
        let Some(class) = AnyClass::get(c"CAMetalLayer") else { return };
        let Some(method) = class.instance_method(sel!(nextDrawable)) else { return };
        let traced: Imp = unsafe { std::mem::transmute(next_drawable_traced as unsafe extern "C-unwind" fn(_, _) -> _) };
        let old = unsafe { method.set_implementation(traced) };
        *ORIGINAL.lock().unwrap() = Some(old);
    });
}

/// 벤치 창을 초점 없이 맨 위에 띄운다 — 가려진 창은 표시가 안 돼 presentedTime 이 0 으로 온다. 사람 초점은
/// 그대로 둔다(활성화하지 않는다).
pub fn float_for_bench(window: &Window) {
    let Some(view) = ns_view(window) else { return };
    let Some(ns_window) = view.window() else { return };
    // NSFloatingWindowLevel
    let _: () = unsafe { msg_send![&*ns_window, setLevel: 3isize] };
}

// ---- 합성 키 -----------------------------------------------------------------------------------

/// 사람 손에서 오는 것과 같은 keyDown/keyUp 을 winit 뷰에 바로 넘긴다. 창 서버를 거치지 않아 사람 화면의
/// 초점은 그대로다.
pub fn inject_key(window: &Window, ch: &str, key_code: u16) {
    let Some(view) = ns_view(window) else { return };
    let Some(ns_window) = view.window() else { return };
    let number = ns_window.windowNumber();
    let text = NSString::from_str(ch);
    for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let ev = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(),
            media_time(),
            number,
            None,
            &text,
            &text,
            false,
            key_code,
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
