//! Starting a thread on a chosen CPU.
//!
//! **Linux pins it; ToyOS has no affinity, so it is staged.** On ToyOS a new
//! thread runs where spawn placement puts it, so candidates are started until
//! one first runs on an accepted CPU and the rest end; what keeps it there is
//! the scheduler, not a mask, and every user of a placed thread reads where its
//! threads actually ran.

use std::thread::JoinHandle;

use crate::sys;

/// A thread that began its body on `cpu`.
pub struct Placed {
    pub thread: JoinHandle<()>,
    pub cpu: u32,
    #[cfg(target_os = "linux")]
    tid: libc::pid_t,
}

impl Placed {
    /// Let the thread run on any CPU from now on. On ToyOS it always could.
    pub fn release(&self) {
        #[cfg(target_os = "linux")]
        sys::unpin(self.tid);
    }
}

/// Start `body` on the first CPU `wanted` accepts.
#[cfg(target_os = "linux")]
pub fn spawn_on(wanted: impl Fn(u32) -> bool, body: impl FnOnce() + Send + 'static) -> Result<Placed, String> {
    use std::sync::mpsc;
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

/// Start `body` on the first CPU `wanted` accepts that a new thread lands
/// on, or say none did within [`STAGE`].
#[cfg(target_os = "toyos")]
pub fn spawn_on(wanted: impl Fn(u32) -> bool, body: impl FnOnce() + Send + 'static) -> Result<Placed, String> {
    use std::sync::mpsc;
    let deadline = std::time::Instant::now() + STAGE;
    let mut body = Some(body);
    let mut tried = 0;
    while std::time::Instant::now() < deadline {
        tried += 1;
        diag::count(&diag::PLACER_ON, sys::current_cpu());
        let (said, heard) = mpsc::channel();
        let (verdict, judged) = mpsc::channel::<Option<Box<dyn FnOnce() + Send>>>();
        let thread = std::thread::spawn(move || {
            said.send(sys::current_cpu()).expect("the placer waits");
            if let Ok(Some(body)) = judged.recv() {
                body();
            }
        });
        let on = heard.recv().map_err(|_| "a candidate ended before it said where it ran")?;
        diag::count(&diag::LANDED_ON, on);
        if wanted(on) {
            let body = body.take().expect("a body is kept once");
            verdict.send(Some(Box::new(body))).map_err(|_| "a kept candidate ended")?;
            return Ok(Placed { thread, cpu: on });
        }
        verdict.send(None).map_err(|_| "a candidate ended before its verdict")?;
        thread.join().map_err(|_| "a candidate let go panicked")?;
    }
    Err(format!("{tried} threads started in {STAGE:?} and none first ran where it was wanted"))
}

/// How long one placement may take: spawn placement rotates over the CPUs,
/// so a machine of `n` usually needs about `n` candidates for any one CPU.
#[cfg(target_os = "toyos")]
const STAGE: std::time::Duration = std::time::Duration::from_secs(5);

/// Measurement only (placement diagnosis, never committed to main): where the
/// placer ran at each try, and where its candidates first ran.
#[cfg(target_os = "toyos")]
pub mod diag {
    use std::sync::atomic::{AtomicU32, Ordering};
    pub const CPUS: usize = 64;
    pub static PLACER_ON: [AtomicU32; CPUS] = [const { AtomicU32::new(0) }; CPUS];
    pub static LANDED_ON: [AtomicU32; CPUS] = [const { AtomicU32::new(0) }; CPUS];
    pub fn count(of: &[AtomicU32; CPUS], cpu: u32) {
        if let Some(slot) = of.get(cpu as usize) {
            slot.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn reset() {
        for slot in PLACER_ON.iter().chain(&LANDED_ON) {
            slot.store(0, Ordering::Relaxed);
        }
    }
    /// `cpu:count` pairs of the nonzero slots, `/`-separated.
    pub fn take(of: &[AtomicU32; CPUS]) -> String {
        let pairs: Vec<String> = of
            .iter()
            .enumerate()
            .filter_map(|(cpu, slot)| match slot.swap(0, Ordering::Relaxed) {
                0 => None,
                n => Some(format!("{cpu}:{n}")),
            })
            .collect();
        if pairs.is_empty() { "-".into() } else { pairs.join("/") }
    }
}
