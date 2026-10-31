//! Loom: a mask setter against a CPU adopting the task it sets.
//!
//! ```text
//! setter                              mover, then adopter
//! ------                              -------------------
//! SharedMask::store  (version)        Ready(A) -> InTransit(B) -> Ready(B)
//! affinity_fence()                    affinity_fence()
//! realign: read state, kick its CPU   read the version
//! ```
//!
//! A store-buffering pair: if both reads missed, the setter would kick A, which
//! no longer holds the task, and B would queue it under the old mask until some
//! later refresh. The control is
//! `cargo test -p kernel-loom --features affinity-fence-relaxed --test loom_affinity`,
//! which must red here.

#![cfg(feature = "loom")]

use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::Arc;
use kernel_loom::sched::affinity::{affinity_fence, CpuMask};
use kernel_loom::sched::cpu::{CpuHandle, CpuHandles};
use kernel_loom::sched::hw::{CpuId, Kicker};
use kernel_loom::sched::mailbox::mailbox;
use kernel_loom::sched::task::{TaskKey, TaskShared, TaskState};
use kernel_loom::sched::topology::Topology;
use kernel_loom::sched_model::{model, Msg, PreemptModel, CPU0, CPU1};

/// The CPUs a kick reached, one bit each.
struct Kicked(AtomicU64);

impl Kicker for Kicked {
    fn kick(&self, target: CpuId) {
        self.0.fetch_or(1 << target.0, Ordering::AcqRel);
    }

    fn runs_on(&self, _cpu: CpuId) -> bool {
        false
    }
}

#[test]
fn a_moved_task_meets_its_new_mask_or_its_cpu_is_kicked() {
    model(|| {
        let (tx0, _rx0) = mailbox::<Msg>();
        let (tx1, _rx1) = mailbox::<Msg>();
        let cpus = Arc::new(CpuHandles::new(vec![CpuHandle::new(CPU0, tx0), CpuHandle::new(CPU1, tx1)], Topology::flat(2)));
        let task = Arc::new(TaskShared::<Msg>::new(TaskKey(1), TaskState::Ready(CPU0), CpuMask::ANY));
        let kicked = Arc::new(Kicked(AtomicU64::new(0)));
        let preempt = PreemptModel::new();

        let adopter = {
            let task = task.clone();
            loom::thread::spawn(move || {
                assert!(task.transition(TaskState::Ready(CPU0), TaskState::InTransit(CPU1)));
                assert!(task.transition(TaskState::InTransit(CPU1), TaskState::Ready(CPU1)));
                affinity_fence();
                task.mask().version()
            })
        };
        task.mask().store(CpuMask::one(CPU0), &preempt.disable());
        cpus.realign(&task, &*kicked);
        let adopted_version = adopter.join().unwrap();

        let cpu1_kicked = kicked.0.load(Ordering::Acquire) & 1 << CPU1.0 != 0;
        assert!(
            cpu1_kicked || adopted_version != 0,
            "cpu1 adopted the task at the old mask's version and the setter kicked only cpu0: \
             neither saw the other",
        );
    });
}
