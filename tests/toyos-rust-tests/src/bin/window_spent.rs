//! A window's event reads never wait on a readiness answer whose frame was
//! already read.
//!
//! A timed `poll_event` that finds nothing leaves the window's own poll armed.
//! The compositor's frame event answers it, and `recv_event` reads that frame
//! without looking at the answer, so the answer is still waiting in the
//! window's ring with nothing behind it. `poll_event(0)` has to say `None` at
//! once, a timed `poll_event` has to wait past the answer to its timeout, and
//! the next frame event has to be read.

use std::process::exit;

use toyos::poller::{Poller, READABLE};
use window::{Event, Window};

/// Long enough for the wait to register its poll, which is all it is for:
/// nothing is sent to a window that has not presented, whatever this is.
const ARM_NANOS: u64 = 1_000_000;

fn fail(what: &str) -> ! {
    println!("WINDOW-SPENT-FAIL {what}");
    exit(1);
}

fn named(event: &Option<Event>) -> &'static str {
    match event {
        None => "nothing",
        Some(Event::KeyInput(_)) => "KeyInput",
        Some(Event::MouseInput(_)) => "MouseInput",
        Some(Event::ClipboardPaste(_)) => "ClipboardPaste",
        Some(Event::Resized) => "Resized",
        Some(Event::Close) => "Close",
        Some(Event::LayoutChanged) => "LayoutChanged",
        Some(Event::Frame) => "Frame",
    }
}

fn main() {
    let mut window = Window::create_with_title(160, 100, "spent").unwrap_or_else(|e| {
        println!("WINDOW-SPENT-REFUSED {e}");
        exit(1);
    });

    let armed = window.poll_event(ARM_NANOS);
    if armed.is_some() {
        fail(&format!("{} arrived before anything was presented", named(&armed)));
    }

    window.present();
    // The frame event's post answers the window's armed poll before this one,
    // which registered after it.
    let arrival = Poller::new(1);
    arrival.watch_raw(window.handle(), READABLE, 0);
    arrival.wait(1, u64::MAX, |_| {});
    drop(arrival);
    let frame = Some(window.recv_event());
    if !matches!(frame, Some(Event::Frame)) {
        fail(&format!("the first present was answered with {}", named(&frame)));
    }

    let at_once = window.poll_event(0);
    if at_once.is_some() {
        fail(&format!("poll_event(0) after the spent answer read {}", named(&at_once)));
    }
    let timed = window.poll_event(ARM_NANOS);
    if timed.is_some() {
        fail(&format!("a timed poll_event after the spent answer read {}", named(&timed)));
    }

    window.present();
    let next = Some(window.recv_event());
    if !matches!(next, Some(Event::Frame)) {
        fail(&format!("the second present was answered with {}", named(&next)));
    }
    println!("WINDOW-SPENT-OK");
}
