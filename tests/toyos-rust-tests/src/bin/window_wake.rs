//! A wake from another thread ends a `window::Waiter` wait that is also
//! watching windows — the wait a winit event loop blocks in, and the one an
//! `EventLoopProxy` has to be able to end.
//!
//! [`WINDOWS`] windows are watched, past the room a new waiter declares, so the
//! waiter has to grow its poller before its first wait can register them. None
//! presents, so nothing arrives on their connections and only the wake can end
//! a wait. The rounds alternate: in one the helper raises the wake and says so
//! before the wait is entered, so the wake is already pending when it starts;
//! in the next the helper raises it only once told the wait is being entered,
//! which lands it either just before the wait blocks or while it is blocked —
//! which of the two is not observed. Every round must end `Ready`, and the wake
//! must be what ended it.
//!
//! Then one wait after more wakes than the wake pipe holds, measured on a
//! fresh pipe: a wake that finds the pipe full is one already pending, so the
//! flood raises nothing but that and ends the next wait as one wake does.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use toyos_abi::syscall::SyscallError;
use window::{Waiter, Window, Woke};

const ROUNDS: u32 = 200;

/// More than a new `Waiter` has room for once the wake is counted.
const WINDOWS: usize = 4;

/// A liveness ceiling, not a duration: a delivered wake ends the wait at once,
/// and only a lost one reaches this.
const CEILING: Duration = Duration::from_secs(10);

fn main() {
    let mut windows: Vec<Window> = (0..WINDOWS)
        .map(|i| match Window::create_with_title(160, 100, &format!("wake {i}")) {
            Ok(w) => w,
            Err(e) => {
                println!("WINDOW-WAKE-REFUSED {e}");
                std::process::exit(1);
            }
        })
        .collect();
    let mut waiter = Waiter::new();
    let waker = waiter.waker();
    let (go, rounds) = mpsc::channel::<()>();
    let (woke, raised) = mpsc::channel::<()>();
    let helper = thread::spawn(move || {
        for () in rounds {
            waker.wake();
            woke.send(()).expect("the main thread outlives the helper's rounds");
        }
    });

    for round in 0..ROUNDS {
        let pending_first = round % 2 == 0;
        go.send(()).expect("the helper is alive until `go` drops");
        if pending_first {
            raised.recv_timeout(CEILING).expect("the helper raised the wake");
        }
        loop {
            if waiter.wait(windows.iter().map(Window::handle), Some(CEILING)) == Woke::TimedOut {
                println!("WINDOW-WAKE-LOST round={round}");
                std::process::exit(1);
            }
            if waiter.take_wake() {
                break;
            }
            // A window said something after all; read it and wait again.
            for window in &mut windows {
                while window.poll_event(0).is_some() {}
            }
        }
        if !pending_first {
            raised.recv_timeout(CEILING).expect("the helper raised the wake");
        }
    }
    drop(go);
    helper.join().expect("the helper does not panic");

    let flood = pipe_capacity() + 1;
    let waker = waiter.waker();
    let raising = Instant::now();
    for _ in 0..flood {
        waker.wake();
    }
    let raised_in = raising.elapsed();
    if waiter.wait(windows.iter().map(Window::handle), Some(CEILING)) == Woke::TimedOut
        || !waiter.take_wake()
    {
        println!("WINDOW-WAKE-LOST after a flood of {flood} wakes");
        std::process::exit(1);
    }
    if waiter.take_wake() {
        println!("WINDOW-WAKE-LOST a flood of {flood} wakes was not taken by one take");
        std::process::exit(1);
    }
    println!("WINDOW-WAKE-OK rounds={ROUNDS} windows={WINDOWS} flood={flood} raised in {raised_in:?}");
}

/// The bytes a fresh pipe holds before a write would block.
fn pipe_capacity() -> usize {
    let (_read, write) = toyos::pipe_pair().expect("a pipe to measure");
    let chunk = [0u8; 64 * 1024];
    let mut held = 0;
    loop {
        match write.write_nonblock(&chunk) {
            Ok(n) => held += n,
            Err(SyscallError::WouldBlock) => return held,
            Err(e) => panic!("measuring a pipe: {e:?}"),
        }
    }
}
