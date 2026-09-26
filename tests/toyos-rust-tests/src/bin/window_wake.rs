//! A wake from another thread ends a `window::Waiter` wait that is also
//! watching a window — the wait a winit event loop blocks in, and the one an
//! `EventLoopProxy` has to be able to end.
//!
//! The window never presents, so nothing arrives on its connection and only
//! the wake can end a wait. A helper thread raises one wake per round the
//! moment it is told to, so across the rounds a wake lands both before the
//! wait blocks and while it is blocked; every round must end `Ready`, and the
//! wake must be what ended it.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use window::{Waiter, Window, Woke};

const ROUNDS: u32 = 200;

/// A liveness ceiling, not a duration: a delivered wake ends the wait at once,
/// and only a lost one reaches this.
const CEILING: Duration = Duration::from_secs(10);

fn main() {
    let mut window = match Window::create_with_title(160, 100, "wake") {
        Ok(w) => w,
        Err(e) => {
            println!("WINDOW-WAKE-REFUSED {e}");
            std::process::exit(1);
        }
    };
    let mut waiter = Waiter::new();
    let waker = waiter.waker();
    let (go, rounds) = mpsc::channel::<()>();
    let helper = thread::spawn(move || {
        for () in rounds {
            waker.wake();
        }
    });

    for round in 0..ROUNDS {
        go.send(()).expect("the helper is alive until `go` drops");
        loop {
            if waiter.wait(std::iter::once(window.handle()), Some(CEILING)) == Woke::TimedOut {
                println!("WINDOW-WAKE-LOST round={round}");
                std::process::exit(1);
            }
            if waiter.take_wake() {
                break;
            }
            // The window said something after all; read it and wait again.
            while window.poll_event(0).is_some() {}
        }
    }
    drop(go);
    helper.join().expect("the helper does not panic");
    println!("WINDOW-WAKE-OK rounds={ROUNDS}");
}
