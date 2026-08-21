//! Enumerating monitors (for the layer-peek's monitor picker) via CoreGraphics.

#![cfg(target_os = "macos")]

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
}
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

/// One monitor in the global (points, top-left origin) coordinate space that
/// egui viewport positions use.
#[derive(Clone, Debug)]
pub struct Monitor {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// The display's localized name (e.g. "Built-in Retina Display").
    pub name: Option<String>,
}

/// Active monitors in CoreGraphics order (index 0 is the main display).
pub fn monitors() -> Vec<Monitor> {
    let names = display_names();
    unsafe {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        if CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) != 0 {
            return Vec::new();
        }
        // `count` is the TOTAL active display count and can exceed our buffer -
        // clamp so `ids[i]` can never index out of bounds.
        let count = (count as usize).min(ids.len());
        (0..count)
            .map(|i| {
                let b = CGDisplayBounds(ids[i]);
                Monitor {
                    x: b.origin.x as f32,
                    y: b.origin.y as f32,
                    w: b.size.width as f32,
                    h: b.size.height as f32,
                    name: names.get(&ids[i]).cloned(),
                }
            })
            .collect()
    }
}

/// Map CGDirectDisplayID → localized name, via `NSScreen`.
fn display_names() -> std::collections::HashMap<u32, String> {
    use objc2::rc::autoreleasepool;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    type Id = *mut AnyObject;
    // This runs every couple of seconds for the lifetime of the app. Every
    // msg_send here (`screens`, each `screen`, `localizedName`, the desc dict,
    // `objectForKey`…) hands back an autoreleased object; drain them at once
    // when we're done instead of relying on the caller's pool being drained.
    autoreleasepool(|_| {
        let mut map = std::collections::HashMap::new();
        unsafe {
            let screens: Id = msg_send![class!(NSScreen), screens];
            if screens.is_null() {
                return map;
            }
            let count: usize = msg_send![screens, count];
            let number_key = NSString::from_str("NSScreenNumber");
            for i in 0..count {
                let screen: Id = msg_send![screens, objectAtIndex: i];
                if screen.is_null() {
                    continue;
                }
                let name_ptr: Id = msg_send![screen, localizedName];
                let name = if name_ptr.is_null() {
                    continue;
                } else {
                    (*(name_ptr as *const NSString)).to_string()
                };
                let desc: Id = msg_send![screen, deviceDescription];
                if desc.is_null() {
                    continue;
                }
                let num: Id = msg_send![desc, objectForKey: &*number_key];
                if num.is_null() {
                    continue;
                }
                let id: u32 = msg_send![num, unsignedIntValue];
                map.insert(id, name);
            }
        }
        map
    })
}
