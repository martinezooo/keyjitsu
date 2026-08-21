//! The transparent on-screen keyboard HUD: a borderless, click-through,
//! always-on-top NSWindow whose content is a CALayer per keycap (plus
//! CATextLayers for legends). No app bundle needed - we run the AppKit event
//! pump by hand from the CLI process.

#![cfg(target_os = "macos")]
#![allow(non_snake_case)]

use objc2::encode::{Encoding, RefEncode};
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::geometry::Geometry;
use crate::legend::KeyLabels;

/// Opaque CoreGraphics color. The encoding must spell `^{CGColor=}` so that
/// objc2's debug signature checks accept it where CGColorRef is expected.
#[repr(C)]
struct CGColor {
    _priv: [u8; 0],
}
unsafe impl RefEncode for CGColor {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}
type CGColorRef = *const CGColor;

#[link(name = "AppKit", kind = "framework")]
extern "C" {}
#[link(name = "QuartzCore", kind = "framework")]
extern "C" {}
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorCreateSRGB(red: f64, green: f64, blue: f64, alpha: f64) -> CGColorRef;
    fn CGColorRelease(color: CGColorRef);
}

type Id = *mut AnyObject;

// AppKit / CoreAnimation constants.
const NS_ACTIVATION_POLICY_ACCESSORY: isize = 1;
const NS_BACKING_STORE_BUFFERED: usize = 2;
const NS_WINDOW_STYLE_BORDERLESS: usize = 0;
const NS_STATUS_WINDOW_LEVEL: isize = 25;
// canJoinAllSpaces | stationary | fullScreenAuxiliary
const COLLECTION_BEHAVIOR: usize = (1 << 0) | (1 << 4) | (1 << 8);

/// Pixels per 1u key at scale 1.0.
const UNIT: f64 = 54.0;
const GAP: f64 = 5.0;
const PAD: f64 = 14.0;
const HEADER: f64 = 26.0;
/// Gap between the overlay and the screen edge it hugs (top/bottom positions).
const EDGE_MARGIN: f64 = 48.0;
/// How long `pump()` lets the AppKit run loop drain events each call.
const PUMP_INTERVAL_SECS: f64 = 0.03;
/// `NSEventMaskAny` - match every event kind.
const NS_EVENT_MASK_ANY: usize = usize::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OverlayPosition {
    Bottom,
    Center,
    Top,
}

pub struct Overlay {
    app: Id,
    window: Id,
    key_layers: Vec<Id>,
    tap_layers: Vec<Id>,
    hold_layers: Vec<Id>,
    title_layer: Id,
    bg_normal: CGColorRef,
    bg_pressed: CGColorRef,
    visible: bool,
}

fn cg(r: f64, g: f64, b: f64, a: f64) -> CGColorRef {
    unsafe { CGColorCreateSRGB(r, g, b, a) }
}

fn set_string(layer: Id, s: &str) {
    let ns = NSString::from_str(s);
    let _: () = unsafe { msg_send![layer, setString: &*ns] };
}

impl Overlay {
    pub fn new(geo: &Geometry, scale: f64, opacity: f64, position: OverlayPosition) -> Overlay {
        autoreleasepool(|_| unsafe { Self::build(geo, scale, opacity, position) })
    }

    unsafe fn build(
        geo: &Geometry,
        scale: f64,
        opacity: f64,
        position: OverlayPosition,
    ) -> Overlay {
        let unit = UNIT * scale;
        let gap = GAP * scale;
        let pad = PAD * scale;
        let header = HEADER * scale;

        let app: Id = msg_send![class!(NSApplication), sharedApplication];
        let _: Bool = msg_send![app, setActivationPolicy: NS_ACTIVATION_POLICY_ACCESSORY];
        let _: () = msg_send![app, finishLaunching];

        // Window size from geometry extents.
        let max_x = geo.keys.iter().map(|k| k.x).fold(0.0f32, f32::max) as f64;
        let max_y = geo.keys.iter().map(|k| k.y).fold(0.0f32, f32::max) as f64;
        let width = (max_x + 1.0) * unit + pad * 2.0;
        let height = (max_y + 1.0) * unit + pad * 2.0 + header;

        let screen: Id = msg_send![class!(NSScreen), mainScreen];
        let sframe: NSRect = msg_send![screen, visibleFrame];
        let x = sframe.origin.x + (sframe.size.width - width) / 2.0;
        let y = match position {
            OverlayPosition::Bottom => sframe.origin.y + EDGE_MARGIN,
            OverlayPosition::Top => sframe.origin.y + sframe.size.height - height - EDGE_MARGIN,
            OverlayPosition::Center => sframe.origin.y + (sframe.size.height - height) / 2.0,
        };
        let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));

        let window: Id = msg_send![class!(NSWindow), alloc];
        let window: Id = msg_send![
            window,
            initWithContentRect: rect,
            styleMask: NS_WINDOW_STYLE_BORDERLESS,
            backing: NS_BACKING_STORE_BUFFERED,
            defer: Bool::NO
        ];
        let clear: Id = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setOpaque: Bool::NO];
        let _: () = msg_send![window, setBackgroundColor: clear];
        let _: () = msg_send![window, setLevel: NS_STATUS_WINDOW_LEVEL];
        let _: () = msg_send![window, setIgnoresMouseEvents: Bool::YES];
        let _: () = msg_send![window, setHasShadow: Bool::NO];
        let _: () = msg_send![window, setCollectionBehavior: COLLECTION_BEHAVIOR];

        let view: Id = msg_send![window, contentView];
        let _: () = msg_send![view, setWantsLayer: Bool::YES];
        let root: Id = msg_send![view, layer];
        let _: () = msg_send![root, setGeometryFlipped: Bool::YES];
        let content_scale: f64 = msg_send![window, backingScaleFactor];

        let bg_normal = cg(0.09, 0.10, 0.14, opacity);
        let bg_pressed = cg(0.55, 0.42, 1.0, (opacity + 0.25).min(1.0));
        let border = cg(1.0, 1.0, 1.0, 0.12);
        let white = cg(1.0, 1.0, 1.0, 0.95);
        let gray = cg(1.0, 1.0, 1.0, 0.55);

        // Header: "L0 · Main".
        let title_layer: Id = msg_send![class!(CATextLayer), new];
        let title_frame = NSRect::new(
            NSPoint::new(pad, 4.0 * scale),
            NSSize::new(width - pad * 2.0, header),
        );
        let _: () = msg_send![title_layer, setFrame: title_frame];
        let _: () = msg_send![title_layer, setFontSize: 14.0 * scale];
        let _: () = msg_send![title_layer, setForegroundColor: white];
        let _: () = msg_send![title_layer, setContentsScale: content_scale];
        let _: () = msg_send![root, addSublayer: title_layer];

        let mut key_layers = Vec::with_capacity(geo.len());
        let mut tap_layers = Vec::with_capacity(geo.len());
        let mut hold_layers = Vec::with_capacity(geo.len());
        let center = NSString::from_str("center");

        for k in &geo.keys {
            let kx = pad + k.x as f64 * unit;
            let ky = header + pad + k.y as f64 * unit;
            let kw = unit - gap;
            let kh = unit - gap;

            let layer: Id = msg_send![class!(CALayer), new];
            let frame = NSRect::new(NSPoint::new(kx, ky), NSSize::new(kw, kh));
            let _: () = msg_send![layer, setFrame: frame];
            let _: () = msg_send![layer, setCornerRadius: 7.0 * scale];
            let _: () = msg_send![layer, setBackgroundColor: bg_normal];
            let _: () = msg_send![layer, setBorderWidth: 1.0];
            let _: () = msg_send![layer, setBorderColor: border];
            let _: () = msg_send![layer, setGeometryFlipped: Bool::YES];
            let _: () = msg_send![root, addSublayer: layer];

            let tap: Id = msg_send![class!(CATextLayer), new];
            let tap_frame = NSRect::new(
                NSPoint::new(2.0, kh * 0.22),
                NSSize::new(kw - 4.0, 18.0 * scale),
            );
            let _: () = msg_send![tap, setFrame: tap_frame];
            let _: () = msg_send![tap, setFontSize: 13.0 * scale];
            let _: () = msg_send![tap, setAlignmentMode: &*center];
            let _: () = msg_send![tap, setForegroundColor: white];
            let _: () = msg_send![tap, setContentsScale: content_scale];
            let _: () = msg_send![layer, addSublayer: tap];

            let hold: Id = msg_send![class!(CATextLayer), new];
            let hold_frame = NSRect::new(
                NSPoint::new(2.0, kh * 0.60),
                NSSize::new(kw - 4.0, 13.0 * scale),
            );
            let _: () = msg_send![hold, setFrame: hold_frame];
            let _: () = msg_send![hold, setFontSize: 9.0 * scale];
            let _: () = msg_send![hold, setAlignmentMode: &*center];
            let _: () = msg_send![hold, setForegroundColor: gray];
            let _: () = msg_send![hold, setContentsScale: content_scale];
            let _: () = msg_send![layer, addSublayer: hold];

            key_layers.push(layer);
            tap_layers.push(tap);
            hold_layers.push(hold);
        }

        Overlay {
            app,
            window,
            key_layers,
            tap_layers,
            hold_layers,
            title_layer,
            bg_normal,
            bg_pressed,
            visible: false,
        }
    }

    /// Swap all legends (on layer change) and the header title.
    pub fn set_labels(&self, labels: &[KeyLabels], glow: &[Option<(u8, u8, u8)>], title: &str) {
        autoreleasepool(|_| unsafe {
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: Bool::YES];
            set_string(self.title_layer, title);
            for (i, l) in labels.iter().enumerate() {
                if let (Some(&tap), Some(&hold)) = (self.tap_layers.get(i), self.hold_layers.get(i))
                {
                    set_string(tap, &l.tap);
                    set_string(hold, l.hold.as_deref().unwrap_or(""));
                    let color = match glow.get(i).copied().flatten() {
                        Some((r, g, b)) => {
                            cg(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 0.95)
                        }
                        None => cg(1.0, 1.0, 1.0, 0.95),
                    };
                    let _: () = msg_send![tap, setForegroundColor: color];
                    CGColorRelease(color);
                }
            }
            let _: () = msg_send![class!(CATransaction), commit];
        });
    }

    pub fn set_pressed(&self, idx: usize, pressed: bool) {
        let Some(&layer) = self.key_layers.get(idx) else { return };
        autoreleasepool(|_| unsafe {
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: Bool::YES];
            let color = if pressed { self.bg_pressed } else { self.bg_normal };
            let _: () = msg_send![layer, setBackgroundColor: color];
            let _: () = msg_send![class!(CATransaction), commit];
        });
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        autoreleasepool(|_| unsafe {
            if visible {
                let _: () = msg_send![self.window, orderFrontRegardless];
            } else {
                let nil: Id = std::ptr::null_mut();
                let _: () = msg_send![self.window, orderOut: nil];
            }
        });
    }

    /// Serve the AppKit event queue; blocks at most ~30 ms. Must be called
    /// regularly from the thread that created the overlay (the main thread).
    pub fn pump(&self) {
        autoreleasepool(|_| unsafe {
            let mode = NSString::from_str("kCFRunLoopDefaultMode");
            let date: Id = msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: PUMP_INTERVAL_SECS];
            loop {
                let ev: Id = msg_send![
                    self.app,
                    nextEventMatchingMask: NS_EVENT_MASK_ANY,
                    untilDate: date,
                    inMode: &*mode,
                    dequeue: Bool::YES
                ];
                if ev.is_null() {
                    break;
                }
                let _: () = msg_send![self.app, sendEvent: ev];
            }
        });
    }
}
