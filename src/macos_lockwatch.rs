//! Keep the keyboard guard from ever locking you out.
//!
//! The guard ([`crate::macos_kb`]) disables the Mac's built-in keyboard while
//! the Voyager is connected, via a `hidutil` remap that SURVIVES sleep. So at
//! the login window after a sleep/lock the built-in is dead and you can only
//! type on the Voyager - if it's unplugged, mid-flash, or on the wrong layer,
//! you're locked out until a reboot.
//!
//! Fix: macOS posts distributed notifications when the screen locks and
//! unlocks. We observe them and bridge to the guard - restore the built-in on
//! lock (login window is always usable), re-disable on unlock. The callbacks
//! no-op unless a guard is actually engaged, so this is safe to install once
//! and leave running regardless of the guard toggle.
#![cfg(target_os = "macos")]

use core::ffi::c_void;

use objc2_foundation::NSString;

#[repr(C)]
struct CfNotificationCenter {
    _private: [u8; 0],
}
type CFNotificationCenterRef = *mut CfNotificationCenter;
type CFStringRef = *const c_void;

/// `void (*)(CFNotificationCenterRef, void *observer, CFStringRef name,
///           const void *object, CFDictionaryRef userInfo)`
type CFNotificationCallback = extern "C" fn(
    CFNotificationCenterRef,
    *mut c_void,
    CFStringRef,
    *const c_void,
    *const c_void,
);

/// `CFNotificationSuspensionBehaviorDeliverImmediately` - don't coalesce/hold.
const DELIVER_IMMEDIATELY: isize = 4;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFNotificationCenterGetDistributedCenter() -> CFNotificationCenterRef;
    fn CFNotificationCenterAddObserver(
        center: CFNotificationCenterRef,
        observer: *const c_void,
        callback: CFNotificationCallback,
        name: CFStringRef,
        object: *const c_void,
        suspension_behavior: isize,
    );
}

extern "C" fn on_locked(
    _center: CFNotificationCenterRef,
    _observer: *mut c_void,
    _name: CFStringRef,
    _object: *const c_void,
    _user_info: *const c_void,
) {
    crate::macos_kb::suspend_for_lock();
}

extern "C" fn on_unlocked(
    _center: CFNotificationCenterRef,
    _observer: *mut c_void,
    _name: CFStringRef,
    _object: *const c_void,
    _user_info: *const c_void,
) {
    crate::macos_kb::resume_after_unlock();
}

/// Register the screen lock/unlock observers. Call once, on the main thread,
/// early in startup; delivery happens on the app's main run loop (eframe pumps
/// it). Idempotent enough - but only call it once.
pub fn install() {
    unsafe {
        let center = CFNotificationCenterGetDistributedCenter();
        if center.is_null() {
            return;
        }
        // NSString is toll-free bridged to CFString. Leak the names so they
        // outlive the observers (install runs exactly once for the app's life).
        let locked = NSString::from_str("com.apple.screenIsLocked");
        let unlocked = NSString::from_str("com.apple.screenIsUnlocked");
        let locked_ref = (&*locked as *const NSString).cast::<c_void>();
        let unlocked_ref = (&*unlocked as *const NSString).cast::<c_void>();
        CFNotificationCenterAddObserver(
            center,
            core::ptr::null(),
            on_locked,
            locked_ref,
            core::ptr::null(),
            DELIVER_IMMEDIATELY,
        );
        CFNotificationCenterAddObserver(
            center,
            core::ptr::null(),
            on_unlocked,
            unlocked_ref,
            core::ptr::null(),
            DELIVER_IMMEDIATELY,
        );
        core::mem::forget(locked);
        core::mem::forget(unlocked);
    }
}
