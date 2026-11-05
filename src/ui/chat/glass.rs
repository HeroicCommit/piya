//! macOS Liquid Glass (Tahoe) / vibrancy for the chat panel.
//!
//! Wraps wry's `WryWebViewParent` container — never detaches WKWebView.
//! Subviews must not be inserted into wry's parent (throws on Tahoe).

#![allow(deprecated, unexpected_cfgs)]

use cocoa::appkit::{NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState};
use cocoa::base::{id, nil, NO, YES};
use cocoa::foundation::NSRect;
use objc::{class, msg_send, sel, sel_impl};
use objc::runtime::Class;
use tao::platform::macos::WindowExtMacOS;
use tao::window::Window;
use wry::WebView;

const CORNER_RADIUS: f64 = 20.0;

/// Transparent window — call before building the WebView.
pub fn prepare(window: &Window) {
    unsafe {
        prepare_window(window.ns_window() as id);
    }
}

/// Wrap wry's parent container in native glass/vibrancy.
pub fn apply(window: &Window, _webview: &WebView) -> bool {
    unsafe {
        let ns_window = window.ns_window() as id;
        if glass_available() {
            wrap_liquid_glass(ns_window).unwrap_or_else(|| wrap_vibrancy(ns_window))
        } else {
            wrap_vibrancy(ns_window)
        }
    }
}

unsafe fn prepare_window(ns_window: id) {
    let clear: id = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![ns_window, setBackgroundColor: clear];
    let _: () = msg_send![ns_window, setOpaque: NO];
    let _: () = msg_send![ns_window, setHasShadow: YES];
}

fn glass_available() -> bool {
    Class::get("NSGlassEffectView").is_some()
}

unsafe fn wrap_liquid_glass(ns_window: id) -> Option<bool> {
    let glass_cls = Class::get("NSGlassEffectView")?;
    let content_view: id = msg_send![ns_window, contentView];
    if content_view == nil {
        return None;
    }

    let frame: NSRect = msg_send![content_view, frame];
    let glass: id = msg_send![glass_cls, alloc];
    let glass: id = msg_send![glass, initWithFrame: frame];
    if glass == nil {
        return None;
    }

    let _: () = msg_send![glass, setCornerRadius: CORNER_RADIUS];
    let _: () = msg_send![glass, setClipsToBounds: YES];
    let _: () = msg_send![glass, setStyle: 0i64]; // regular

    let tint: id = msg_send![
        class!(NSColor),
        colorWithCalibratedRed: 0.925
        green: 0.282
        blue: 0.6
        alpha: 0.12
    ];
    if tint != nil {
        let _: () = msg_send![glass, setTintColor: tint];
    }

    mount_content(content_view, frame);
    let _: () = msg_send![glass, setContentView: content_view];
    let _: () = msg_send![ns_window, setContentView: glass];
    Some(true)
}

unsafe fn wrap_vibrancy(ns_window: id) -> bool {
    let content_view: id = msg_send![ns_window, contentView];
    if content_view == nil {
        return false;
    }

    let frame: NSRect = msg_send![content_view, frame];

    let effect: id = msg_send![class!(NSVisualEffectView), alloc];
    let effect: id = msg_send![effect, initWithFrame: frame];
    if effect == nil {
        return false;
    }

    let _: () = msg_send![effect, setMaterial: NSVisualEffectMaterial::HudWindow];
    let _: () = msg_send![effect, setBlendingMode: NSVisualEffectBlendingMode::BehindWindow];
    let _: () = msg_send![effect, setState: NSVisualEffectState::Active];
    let _: () = msg_send![effect, setWantsLayer: YES];

    let layer: id = msg_send![effect, layer];
    if layer != nil {
        let _: () = msg_send![layer, setCornerRadius: CORNER_RADIUS];
        let _: () = msg_send![layer, setMasksToBounds: YES];
    }

    mount_content(content_view, frame);
    let _: () = msg_send![effect, addSubview: content_view];
    let _: () = msg_send![ns_window, setContentView: effect];
    true
}

unsafe fn mount_content(content_view: id, frame: NSRect) {
    let _: () = msg_send![content_view, setFrame: frame];
    let _: () = msg_send![content_view, setAutoresizingMask: 18u64];
}
