//! Starting a thread on a chosen CPU: it pins itself there before its body
//! runs, and says where it ran, so a pin the kernel did not hold is a refusal
//! rather than a row measured somewhere else.

use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::sys;

/// A thread that began its body on `cpu`.
pub struct Placed {
    pub thread: JoinHandle<()>,
    pub cpu: u32,
    tid: sys::Tid,
}

impl Placed {
    /// Let the thread run on any CPU this process may from now on.
    pub fn release(&self) {
        sys::unpin(self.tid);
    }
}

/// Start `body` on the first CPU this process may run on that `wanted` accepts.
pub fn spawn_on(wanted: impl Fn(u32) -> bool, body: impl FnOnce() + Send + 'static) -> Result<Placed, String> {
    let target = sys::allowed_cpus()
        .into_iter()
        .map(|cpu| cpu as u32)
        .find(|&cpu| wanted(cpu))
        .ok_or("no CPU this process may run on is wanted")?;
    let (said, heard) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        sys::pin_self(target as usize);
        let on = sys::current_cpu();
        said.send((on, sys::tid())).expect("the placer waits");
        if on == target {
            body();
        }
    });
    let (on, tid) = heard.recv().map_err(|_| "a placed thread ended before it said where it ran")?;
    if on != target {
        thread.join().map_err(|_| "a misplaced thread panicked")?;
        return Err(format!("pinned to CPU {target} and ran on {on}"));
    }
    Ok(Placed { thread, cpu: on, tid })
}
