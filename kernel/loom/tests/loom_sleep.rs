//! Loom: the sleep handshake and the doorbell's kick accounting
//! (invariant I2).
//!
//! The handshake is: publish SLEEPING, drain, check the mailbox one last
//! time, then halt. A producer kicks on the doorbell's 0→1 edge, busy target
//! or asleep, and the one behind it coalesces into that kick. The IPI is
//! modelled by its one effect, the target's `need_resched`: the pass clears
//! it before it begins, as the kernel's `sched::driver::pass` does, and the
//! halt decision reads it last, as the driver's final look before `hlt` does.
//! A CPU that halts with work queued must have one raised after that look —
//! an IPI still on its way to wake it.
//!
//! Two producers, so the edge-coalescing rule is exercised rather than
//! assumed: the second can elide only into a kick that may already have been
//! spent.
//!
//! The edge the property rests on is the release half of `ring`'s
//! read-modify-write: a producer that coalesces leaves its message for the
//! pass the earlier kick buys, and only `begin_pass`'s acquire of the edge it
//! left puts that message in the drain. On x86 every read-modify-write is a
//! full fence, so a build with it relaxed behaves identically to this one and
//! no guest test can fail here. The negative case is a cargo feature rather
//! than a comment:
//!
//! ```text
//! cargo test -p kernel-loom --features doorbell-kick-relaxed \
//!   --test loom_sleep
//! ```
//!
//! makes that read-modify-write relaxed and this file must red, at
//! [`a_halted_cpu_with_queued_work_was_kicked`], whose assertion names the
//! failure exactly: halted with messages still queued and no IPI in flight.
//! A demanded pass rests on the same edge, and
//! [`a_pass_that_answers_a_demand_reads_what_its_demander_wrote`] must red
//! with it.

#![cfg(feature = "loom")]

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::Arc;
use kernel_loom::sched::mailbox::{
    mailbox, Doorbell, Kick, MailboxConsumer, MailboxNode, MailboxProducer, Urgency,
};
use kernel_loom::sched::affinity::MASK_WORDS;
use kernel_loom::sched::cpu::{CpuHandle, CpuHandles};
use kernel_loom::sched::hw::CpuId;
use kernel_loom::sched::topology::Topology;
use kernel_loom::sched_model::{model, Msg, PreemptModel, RemoteGuard};

struct World {
    tx: MailboxProducer<Msg>,
    doorbell: Doorbell,
    preempt: PreemptModel,
    nodes: [MailboxNode<Msg>; 2],
    ipis: AtomicUsize,
    /// The target's `need_resched`, which is all an IPI does here.
    resched: AtomicBool,
}

impl World {
    fn new(tx: MailboxProducer<Msg>) -> Self {
        Self {
            tx,
            doorbell: Doorbell::new(),
            preempt: PreemptModel::new(),
            nodes: [MailboxNode::new(), MailboxNode::new()],
            ipis: AtomicUsize::new(0),
            resched: AtomicBool::new(false),
        }
    }

    fn produce(&self, which: usize) {
        self.tx.post(
            self.nodes[which].claim().expect("one message per node"),
            Msg::Probe(which as u32),
            &RemoteGuard,
        );
        if self.doorbell.ring(Urgency::Normal) == Kick::Send {
            self.ipis.fetch_add(1, Ordering::AcqRel);
            self.resched.store(true, Ordering::SeqCst);
        }
    }

    /// One scheduler pass, ending in the idle disposition. Returns whether
    /// the CPU would halt.
    fn pass(&self, rx: &mut MailboxConsumer<Msg>, drained: &mut Vec<Msg>) -> bool {
        let guard = self.preempt.disable();
        self.resched.store(false, Ordering::SeqCst);
        self.doorbell.begin_pass();
        while let Some(msg) = rx.pop(&guard) {
            drained.push(msg);
        }
        self.doorbell.arm_sleep().confirm(rx).is_ok() && !self.resched.load(Ordering::SeqCst)
    }
}

/// I2: if the CPU halts while work is still queued, an IPI must be on its way
/// that its last look did not see — otherwise nothing would ever wake it.
#[test]
fn a_halted_cpu_with_queued_work_was_kicked() {
    model(|| {
        let (tx, mut rx) = mailbox::<Msg>();
        let world = Arc::new(World::new(tx));

        let producers: Vec<_> = (0..2)
            .map(|which| {
                let world = world.clone();
                loom::thread::spawn(move || world.produce(which))
            })
            .collect();

        let mut drained = Vec::new();
        let halted = world.pass(&mut rx, &mut drained);

        for producer in producers {
            producer.join().unwrap();
        }

        if halted && drained.len() < 2 {
            assert!(
                world.resched.load(Ordering::SeqCst),
                "halted with {} of 2 messages queued and no IPI in flight — \
                 a sleep-through",
                2 - drained.len(),
            );
        }

        // The wake-up pass drains the rest; nothing is lost and every node is
        // free again (otherwise the nodes' drop bomb fires at teardown).
        world.pass(&mut rx, &mut drained);
        drained.sort_by_key(|m| match m {
            Msg::Probe(n) => *n,
            other => panic!("unexpected message {other:?}"),
        });
        assert_eq!(drained, [Msg::Probe(0), Msg::Probe(1)]);
        assert!(world.nodes.iter().all(|n| !n.in_flight()));
    });
}

/// The handshake's liveness in the model: a halted CPU is always brought back
/// by the edge or the IPI, and the pass it then runs drains everything. The
/// bound of three passes is the model's, not the protocol's — a fourth would
/// only re-check an already-empty queue.
#[test]
fn a_kicked_cpu_comes_back_and_drains_everything() {
    model(|| {
        let (tx, mut rx) = mailbox::<Msg>();
        let world = Arc::new(World::new(tx));

        let producers: Vec<_> = (0..2)
            .map(|which| {
                let world = world.clone();
                loom::thread::spawn(move || world.produce(which))
            })
            .collect();

        let mut drained = Vec::new();
        for _ in 0..3 {
            if world.pass(&mut rx, &mut drained) && drained.len() == 2 {
                break;
            }
        }
        for producer in producers {
            producer.join().unwrap();
        }
        world.pass(&mut rx, &mut drained);

        assert_eq!(drained.len(), 2, "every posted message is delivered");
        assert!(world.nodes.iter().all(|n| !n.in_flight()));
        assert!(
            world.ipis.load(Ordering::SeqCst) <= 2,
            "at most one IPI per post; coalesced edges send none",
        );
    });
}

/// **A steal probe is a bit and a ring, and never lost.** A thief asks while
/// the victim begins a pass and takes its asks: either that pass takes the
/// ask, or the ring is still pending and owes the victim another pass, whose
/// take finds it. Ringing before the bit is set (`ask-rings-first`) lets the
/// pass the ring owed take nothing and clear the ring, and the ask waits on a
/// CPU nothing will wake.
#[test]
fn an_ask_is_taken_by_the_pass_its_ring_owes() {
    model(|| {
        let (tx, _rx) = mailbox::<Msg>();
        let handles = Arc::new(CpuHandles::new(vec![CpuHandle::new(CpuId(0), tx)], Topology::flat(1)));
        let thief = CpuId(5);

        let asking = {
            let handles = handles.clone();
            loom::thread::spawn(move || {
                let _ = handles.get(CpuId(0)).ask(thief);
            })
        };
        let victim = handles.get(CpuId(0));
        victim.doorbell().begin_pass();
        let first = victim.take_asks();
        asking.join().unwrap();

        let taken = |asks: [u64; MASK_WORDS]| asks[0] & 1 << 5 != 0;
        if taken(first) {
            return;
        }
        assert!(victim.doorbell().kick_pending(), "the ask was neither taken nor owed a pass: it is lost");
        victim.doorbell().begin_pass();
        assert!(taken(victim.take_asks()), "the pass the ask's ring owed did not find it");
    });
}

/// The machine's stop against one CPU's pass. The stop writes its gate, then
/// demands a pass; once it reads the demand answered, what that CPU reads
/// after its pass is the gate open. The gate is relaxed on both sides, so the
/// doorbell's edge alone carries it.
#[test]
fn a_pass_that_answers_a_demand_reads_what_its_demander_wrote() {
    model(|| {
        let gate = Arc::new(AtomicBool::new(false));
        let doorbell = Arc::new(Doorbell::new());

        let stop = {
            let (gate, doorbell) = (gate.clone(), doorbell.clone());
            loom::thread::spawn(move || {
                gate.store(true, Ordering::Relaxed);
                let _ = doorbell.demand_pass();
                !doorbell.pass_demanded()
            })
        };

        doorbell.begin_pass();
        let open = gate.load(Ordering::Relaxed);

        if stop.join().unwrap() {
            assert!(open, "a pass answered the demand and read the gate closed after it");
        }
    });
}
