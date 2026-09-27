//! What one harness task reserved of the host's vCPUs, and every guest it boots
//! counted against that.
//!
//! A task's guest slot (`buildlock::guest_slot`) is sized before it boots
//! anything, from a table written by hand, and this is the table's backstop in
//! both directions. A boot that would take its task past the reservation is
//! refused before its QEMU is spawned, and so is a boot on a thread no task
//! claimed: either is a guest the host's budget never counted. And the most a
//! task ever had up is kept, so a task that finished green without reaching its
//! reservation is refused too ([`Claimed::unreached`]): it held units idle that
//! another run was waiting for.
//!
//! Per thread, because every guest a task boots is booted on the worker that
//! took the task.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

struct Claim {
    what: String,
    vcpus: u32,
    live: AtomicU32,
    peak: AtomicU32,
}

thread_local! {
    static CLAIM: RefCell<Option<Arc<Claim>>> = const { RefCell::new(None) };
}

/// This thread's claim, from [`claim`] until it drops.
#[must_use]
pub struct Claimed(Arc<Claim>);

/// Declare that the task about to run on this thread has at most `vcpus` vCPUs
/// up at once — the width its host slot was taken for. Zero is a task that
/// boots nothing.
pub fn claim(what: &str, vcpus: u32) -> Claimed {
    let claim = Arc::new(Claim {
        what: what.to_string(),
        vcpus,
        live: AtomicU32::new(0),
        peak: AtomicU32::new(0),
    });
    CLAIM.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none(), "{what}: this thread already runs a task that claimed vCPUs");
        *slot = Some(Arc::clone(&claim));
    });
    Claimed(claim)
}

impl Claimed {
    /// Why the task's reservation was wider than anything it had up, if it
    /// was. Asked of a task that finished green: a red one may have stopped
    /// before its widest moment.
    pub fn unreached(&self) -> Option<String> {
        let peak = self.0.peak.load(Ordering::SeqCst);
        (peak < self.0.vcpus).then(|| {
            format!(
                "[qemu] {}: its host slot was taken for {} vCPUs and it never had more than {peak} \
                 up. Declare the task's widest moment — the vCPUs of every guest it has up at \
                 once — in `tests/toyos.rs`'s VCPUS.",
                self.0.what, self.0.vcpus
            )
        })
    }
}

impl Drop for Claimed {
    fn drop(&mut self) {
        CLAIM.with(|slot| slot.borrow_mut().take());
    }
}

/// One guest's vCPUs, counted against its task's claim until it drops.
#[must_use]
pub struct Hold {
    claim: Arc<Claim>,
    smp: u32,
}

/// Count an `smp`-vCPU guest against this thread's claim, or refuse it by name.
pub fn hold(smp: u32) -> Result<Hold, String> {
    let claim = CLAIM.with(|slot| slot.borrow().clone()).ok_or_else(|| {
        format!(
            "[qemu] an smp={smp} guest was booted on a thread no task claimed vCPUs on, so the \
             host's guest budget does not count it"
        )
    })?;
    let live = claim.live.fetch_add(smp, Ordering::SeqCst) + smp;
    if live > claim.vcpus {
        claim.live.fetch_sub(smp, Ordering::SeqCst);
        return Err(format!(
            "[qemu] {}: this smp={smp} boot would have {live} vCPUs up for a task whose host slot \
             was taken for {}. Declare the task's widest moment — the vCPUs of every guest it \
             has up at once — in `tests/toyos.rs`'s VCPUS.",
            claim.what, claim.vcpus
        ));
    }
    claim.peak.fetch_max(live, Ordering::SeqCst);
    Ok(Hold { claim, smp })
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.claim.live.fetch_sub(self.smp, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The count is per task, not per boot: a second guest up beside the
    /// first is counted with it, and the one that would pass the width is
    /// refused, naming the task.
    #[test]
    fn a_second_boot_past_the_width_is_refused() {
        let _task = claim("a four-vCPU task", 4);
        let first = hold(2).expect("the first guest fits");
        let second = hold(2).expect("the second guest fits beside it");
        let why = hold(2).err().expect("a third two-vCPU guest was admitted to a four-vCPU task");
        assert!(
            why.contains("a four-vCPU task") && why.contains("would have 6 vCPUs up"),
            "the refusal does not name the task and the count: {why}"
        );
        drop((first, second));
        assert!(hold(8).is_err(), "one guest wider than its task was admitted");
    }

    /// A boot with no claim on its thread is one the budget never counted.
    #[test]
    fn a_boot_on_an_unclaimed_thread_is_refused() {
        let why = std::thread::spawn(|| hold(2).err())
            .join()
            .unwrap()
            .expect("a guest was admitted on a thread no task claimed");
        assert!(why.contains("no task claimed"), "{why}");

        drop(claim("a task that has ended", 8));
        assert!(hold(2).is_err(), "a guest was admitted under a claim that had dropped");
    }

    /// A guest that is gone gives its vCPUs back to its task.
    #[test]
    fn a_dropped_boot_gives_its_vcpus_back() {
        let _task = claim("a two-vCPU task", 2);
        for _ in 0..3 {
            let guest = hold(2).expect("a guest as wide as its task, with nothing else up");
            drop(guest);
        }
    }

    /// The reservation is exact from below too: a task that never had its
    /// width up held units nobody used, and one that did, or that claimed
    /// nothing and booted nothing, is fine.
    #[test]
    fn a_task_below_its_claim_is_refused() {
        let wide = claim("an eight-vCPU task", 8);
        drop(hold(2).unwrap());
        let why = wide.unreached().expect("a task that had 2 of its 8 vCPUs up passed");
        assert!(why.contains("an eight-vCPU task") && why.contains("never had more than 2"), "{why}");
        drop(wide);

        let exact = claim("a four-vCPU task", 4);
        let (a, b) = (hold(2).unwrap(), hold(2).unwrap());
        drop((a, b));
        assert_eq!(exact.unreached(), None, "a task that reached its width was refused");
        drop(exact);

        let none = claim("a task with no guest", 0);
        assert_eq!(none.unreached(), None);
        assert!(hold(1).is_err(), "a task that claimed no vCPU booted a guest");
    }
}
