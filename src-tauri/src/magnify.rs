//! M4b commit 1 — the pinch spike (decision Z1; `_handover/m4b-plan.md` § 2). **Removable**: this
//! file, `mod magnify;` in `lib.rs` and one call in `panel::setup`.
//!
//! Step 0 measured that a trackpad pinch never reaches the webview: WebKit fires no `gesture*`
//! event and no `wheel` with `ctrlKey`, wry 0.55.1 sets nothing for magnification on macOS, and
//! tao 0.35.3 has no magnify event at all. So the pinch is caught **natively**: an
//! `NSMagnificationGestureRecognizer` on the popover's content view, whose target logs every
//! state change together with the focus state. Nothing reads it yet — commit 8 wires it to the
//! map's view session if the gate passes.
//!
//! The gate (FOR MARTÍN, TextEdit frontmost with a caret, the popover expanded, a pinch out and
//! in over the band): `began … changed × n … ended` lines with a monotone `magnification` while
//! the panel is key, `frontmost` unchanged on every line — and (review P4) Q1's click, scroll
//! and drag reaching the page as in Step 0 with the recognizer installed. The recognizer's
//! `delays*` properties are logged at install and every one set false: a delayed click is exactly
//! what P4 forbids. FAIL on either half → this commit is reverted and the controls ship alone.
//!
//! Location: `locationInView:` of `nil` is in the window's coordinates, bottom-left origin; the
//! log converts to the panel's top-left origin (`frame.height − y`), the space `panel.rs` and
//! `band_rect` use.

use tauri_nspanel::objc2::rc::Retained;
use tauri_nspanel::objc2::runtime::AnyObject;
use tauri_nspanel::objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use tauri_nspanel::objc2_app_kit::{
    NSApplication, NSGestureRecognizer, NSGestureRecognizerState, NSMagnificationGestureRecognizer,
    NSWindow, NSWorkspace,
};
use tauri_nspanel::objc2_foundation::{NSObject, NSObjectProtocol};

define_class!(
    // The recognizer's action target: one method, one log line per state change.
    #[unsafe(super(NSObject))]
    #[name = "OndarMagnifyTarget"]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct MagnifyTarget;

    unsafe impl NSObjectProtocol for MagnifyTarget {}

    impl MagnifyTarget {
        #[unsafe(method(handleMagnify:))]
        fn handle_magnify(&self, recognizer: &NSMagnificationGestureRecognizer) {
            log_event(recognizer);
        }
    }
);

impl MagnifyTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance of this class.
        unsafe { msg_send![super(this), init] }
    }
}

/// Install the recognizer on `window`'s content view. Main thread only (`panel::setup` runs
/// there); a call off it logs and returns.
pub fn install(window: &NSWindow) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("panel magnify: not on the main thread; recognizer not installed");
        return;
    };
    let Some(view) = window.contentView() else {
        log::warn!("panel magnify: no content view; recognizer not installed");
        return;
    };
    let target = MagnifyTarget::new(mtm);
    let any: &AnyObject = &target;
    // SAFETY: the target responds to `handleMagnify:` (defined above) with the recognizer as its
    // one argument, which is the action signature AppKit sends.
    let recognizer = unsafe {
        NSMagnificationGestureRecognizer::initWithTarget_action(
            NSMagnificationGestureRecognizer::alloc(mtm),
            Some(any),
            Some(sel!(handleMagnify:)),
        )
    };
    let r: &NSGestureRecognizer = &recognizer;
    let before = delays(r);
    r.setDelaysPrimaryMouseButtonEvents(false);
    r.setDelaysSecondaryMouseButtonEvents(false);
    r.setDelaysOtherMouseButtonEvents(false);
    r.setDelaysKeyEvents(false);
    r.setDelaysMagnificationEvents(false);
    r.setDelaysRotationEvents(false);
    view.addGestureRecognizer(r);
    // The recognizer does not retain its target, and there is one popover for the life of the
    // process: the target lives as long as it (a spike; commit 8 gives it an owner).
    std::mem::forget(target);
    log::info!(
        "panel magnify recognizer installed view={} delays_before={before} delays_after={}",
        view.class().name().to_string_lossy(),
        delays(r)
    );
}

/// The six `delays*` flags as `primary/secondary/other/key/magnification/rotation`.
fn delays(r: &NSGestureRecognizer) -> String {
    format!(
        "{}/{}/{}/{}/{}/{}",
        r.delaysPrimaryMouseButtonEvents(),
        r.delaysSecondaryMouseButtonEvents(),
        r.delaysOtherMouseButtonEvents(),
        r.delaysKeyEvents(),
        r.delaysMagnificationEvents(),
        r.delaysRotationEvents()
    )
}

fn log_event(recognizer: &NSMagnificationGestureRecognizer) {
    let r: &NSGestureRecognizer = recognizer;
    let state = match r.state() {
        NSGestureRecognizerState::Possible => "possible",
        NSGestureRecognizerState::Began => "began",
        NSGestureRecognizerState::Changed => "changed",
        NSGestureRecognizerState::Ended => "ended",
        NSGestureRecognizerState::Cancelled => "cancelled",
        NSGestureRecognizerState::Failed => "failed",
        _ => "other",
    };
    let p = r.locationInView(None);
    let window = r.view().and_then(|v| v.window());
    let (y_top_left, key) = match &window {
        Some(w) => (w.frame().size.height - p.y, w.isKeyWindow()),
        None => (f64::NAN, false),
    };
    let (active, frontmost) = match MainThreadMarker::new() {
        Some(mtm) => (
            NSApplication::sharedApplication(mtm).isActive(),
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .and_then(|a| a.bundleIdentifier())
                .map_or_else(|| "none".to_string(), |s| s.to_string()),
        ),
        None => (false, "not-main-thread".to_string()),
    };
    log::info!(
        "panel magnify state={state} magnification={:.4} location=({:.1},{:.1}) key={key} \
         app_active={active} frontmost={frontmost}",
        recognizer.magnification(),
        p.x,
        y_top_left
    );
}
