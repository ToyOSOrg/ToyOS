//! A window client a shell can launch, for the question the T14 asked twice:
//! **when a windowed child's window goes, does its shell get the prompt back?**
//!
//! The owner opened snake from a shell, closed its window with the X button,
//! and never saw a prompt again. The log says snake exited cleanly, and then
//! the shell exited 34 ms later and the terminal 13 ms after that — so the
//! chain came down and the interesting question is which link pulled first.
//!
//! This stands in for snake and answers it without winit in the way: it is a
//! `window::Window` and nothing else, so a reproduction here is about the
//! shell, the terminal and the window protocol, and a *failure* to reproduce
//! narrows it to what winit does that this does not.
//!
//! Two modes, and the difference between them is the whole design:
//!
//! - `exit` — paint, print, leave. The window goes because the process does.
//! - default — paint, print, and wait for the compositor to take the window
//!   away, which is the owner's case: the process is alive when its window is
//!   closed.

use window::{Color, Event, Window};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 200;

fn main() {
    let leave_now = std::env::args().nth(1).as_deref() == Some("exit");

    let mut window = match Window::create_with_title(WIDTH, HEIGHT, "child") {
        Ok(w) => w,
        Err(e) => {
            println!("WINDOW-CHILD-REFUSED {e}");
            std::process::exit(1);
        }
    };
    let fb = window.framebuffer();
    fb.fill_rect(0, 0, WIDTH as usize, HEIGHT as usize, Color { r: 0x20, g: 0x60, b: 0x30 });
    window.present();
    println!("WINDOW-CHILD-UP");

    // No deadline: a window nothing ever closes is a hang the host's ceiling
    // reds.
    if !leave_now {
        while !matches!(window.poll_event(u64::MAX), Some(Event::Close)) {}
    }

    println!("WINDOW-CHILD-GONE");
}
