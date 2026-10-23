//! A window asked for in logical pixels is given a buffer at the display's
//! scale, and a scale set on the display port relays it at the new one.
//!
//! **What it fills its window with is the evidence**: the harness finds the
//! fill on the panel and measures it, which this program cannot.
//!
//! No argument: the window, its size and scale said, its fill on the panel,
//! and the end. `relay`: the same, then Enter, then `screen scale 1` through
//! the launcher (`toybox` holds the display port), the resize that relays the
//! window, its new fill on the panel, then Enter again. The harness presses
//! Enter only once it has read the panel, so neither step outruns it.

use std::os::toyos::process::CommandExt;
use std::process::Command;

use window::{Color, Event, Size, Window};

/// The window's logical size.
const LOGICAL: (u32, u32) = (200, 100);
/// The fills, before and after the relay: colours no chrome or wallpaper has.
const BEFORE: Color = Color { r: 0x00, g: 0xff, b: 0x7f };
const AFTER: Color = Color { r: 0xff, g: 0x00, b: 0x7f };
/// HID usage of Enter.
const ENTER: u8 = 0x28;

fn main() {
    let relay = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("relay") => true,
        Some(other) => panic!("scale_window: unknown argument {other:?}"),
    };
    let size = Size::Logical { width: LOGICAL.0, height: LOGICAL.1 };
    let mut window =
        Window::create_with_title(size, "scale").unwrap_or_else(|e| panic!("scale_window: no window: {e}"));
    show(&mut window, BEFORE);
    if !relay {
        return;
    }
    enter(&mut window);
    let status = Command::new("/system/bin/toybox")
        .independent()
        .args(["screen", "scale", "1"])
        .status()
        .unwrap_or_else(|e| panic!("scale_window: `screen scale 1` was not launched: {e}"));
    assert!(status.success(), "scale_window: `screen scale 1` ended {status:?}");
    loop {
        match window.recv_event() {
            Event::Resized => break,
            Event::Close => panic!("scale_window: the window closed before it was relaid"),
            _ => {}
        }
    }
    show(&mut window, AFTER);
    enter(&mut window);
}

/// Fill the window, present it, and say so once the compositor has put the
/// frame on the panel.
fn show(window: &mut Window, fill: Color) {
    let fb = window.framebuffer();
    fb.fill_rect(0, 0, fb.width(), fb.height(), fill);
    window.present();
    loop {
        match window.recv_event() {
            Event::Frame => break,
            Event::Close => panic!("scale_window: the window closed before its frame was shown"),
            _ => {}
        }
    }
    println!(
        "scale_window: logical {}x{} drawn {}x{} at scale {}",
        LOGICAL.0,
        LOGICAL.1,
        window.width(),
        window.height(),
        window.scale()
    );
}

/// Wait for Enter's press.
fn enter(window: &mut Window) {
    loop {
        match window.recv_event() {
            Event::KeyInput(key) if key.keycode == ENTER && key.pressed() => return,
            Event::Close => panic!("scale_window: the window closed waiting for Enter"),
            _ => {}
        }
    }
}
