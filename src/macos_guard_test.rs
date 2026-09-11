//! A functional test for the keyboard guard: actually listens (read-only,
//! system-wide) for a moment instead of trusting hidutil's own report.
//!
//! hidutil can report a remap as fully applied while the built-in keyboard
//! still delivers key presses: confirmed empirically on a real Mac, `hidutil
//! property --set` only reaches the "Service" HID layer
//! (`AppleHIDKeyboardEventDriverV2`, visible via `hidutil list`), while the
//! built-in keyboard also has a separate raw "Device" layer
//! (`AppleHIDTransportHIDDevice`) that hidutil cannot write to and that can
//! still deliver key presses straight into the system's event stream. So
//! "hidutil says applied" is necessary but not sufficient - this is the only
//! way to get a real answer, short of the user simply noticing a key still
//! works.
//!
//! Needs the "Input Monitoring" permission (System Settings > Privacy &
//! Security > Input Monitoring): a listen-only event tap is still gated by
//! that on modern macOS. Only ever requested when the user clicks "Test the
//! guard" - never on startup - and the tap is read-only and torn down right
//! after the test, so it can't itself interfere with typing.
#![cfg(target_os = "macos")]

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFMachPortRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;

/// `kCGHIDEventTap` - closest to hardware, so a leak from a lower HID layer
/// than hidutil can reach still shows up here.
const CG_HID_EVENT_TAP: u32 = 0;
const CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
/// `kCGEventTapOptionListenOnly` - never modifies or drops events.
const CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
/// `kCGEventKeyDown`.
const CG_EVENT_KEY_DOWN: u64 = 10;

type CGEventTapCallBack = extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(allocator: CFAllocatorRef, port: CFMachPortRef, order: isize) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopDefaultMode: CFStringRef;
}

/// Listen-only: just flags that a key-down arrived. Never touches `event`.
extern "C" fn on_key_event(_proxy: CGEventTapProxy, _event_type: u32, event: CGEventRef, user_info: *mut c_void) -> CGEventRef {
    if !user_info.is_null() {
        unsafe { &*(user_info as *const AtomicBool) }.store(true, Ordering::SeqCst);
    }
    event
}

/// Outcome of [`test_builtin_leaks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardTestOutcome {
    /// No key-down reached the system-wide event stream during the window.
    Blocked,
    /// At least one key-down got through while the test was listening.
    Leaked,
    /// Couldn't listen at all - Input Monitoring permission is missing.
    PermissionNeeded,
}

/// Listen system-wide, read-only, for `duration` and report whether any
/// key-down reached the event stream. Blocking - call it off the UI thread
/// (see [`spawn_test`]).
pub fn test_builtin_leaks(duration: Duration) -> GuardTestOutcome {
    let seen = AtomicBool::new(false);
    let seen_ptr = &seen as *const AtomicBool as *mut c_void;

    let tap = unsafe {
        CGEventTapCreate(
            CG_HID_EVENT_TAP,
            CG_HEAD_INSERT_EVENT_TAP,
            CG_EVENT_TAP_OPTION_LISTEN_ONLY,
            1u64 << CG_EVENT_KEY_DOWN,
            on_key_event,
            seen_ptr,
        )
    };
    if tap.is_null() {
        return GuardTestOutcome::PermissionNeeded;
    }

    unsafe {
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        let rl = CFRunLoopGetCurrent();
        CFRunLoopAddSource(rl, source, kCFRunLoopDefaultMode);
        CGEventTapEnable(tap, true);

        let deadline = Instant::now() + duration;
        while Instant::now() < deadline && !seen.load(Ordering::SeqCst) {
            let slice = (deadline - Instant::now()).min(Duration::from_millis(200));
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, slice.as_secs_f64(), 1);
        }

        CGEventTapEnable(tap, false);
        CFRunLoopRemoveSource(rl, source, kCFRunLoopDefaultMode);
        CFRelease(source as *const c_void);
        CFRelease(tap as *const c_void);
    }

    if seen.load(Ordering::SeqCst) {
        GuardTestOutcome::Leaked
    } else {
        GuardTestOutcome::Blocked
    }
}

/// Run the test on a background thread; poll the receiver from the GUI tick.
pub fn spawn_test(duration: Duration) -> Receiver<GuardTestOutcome> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(test_builtin_leaks(duration));
    });
    rx
}
