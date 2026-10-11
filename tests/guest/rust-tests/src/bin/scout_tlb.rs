//! Measurement scout, never landed: what a cross-process IPC round trip costs
//! as its working set grows, and how many TLB misses each side takes per round
//! trip — read against a kernel twin that keeps or drops PCID, so what the
//! full flush costs is a measured difference.
//!
//! Two processes, this one and a child of itself, ping-pong one byte over a
//! pipe pair (the transport `toyos::ipc` frames). After each byte arrives the
//! receiver touches its working set: one cache line in each of `k` 2 MiB
//! pages, the lines on distinct L1 sets, so `k` sets the TLB entries the round
//! trip needs and barely moves the cache footprint.
//!
//! Printed per `k`: cycles per round trip over `REPS` repetitions of `ROUNDS`
//! (min, median, max); each side's four counters per round trip, summed over
//! every repetition, where the scout kernel programmed them; and the CPUs the
//! diary saw each process picked on, since nothing pins either.

#[path = "../scout.rs"]
mod scout;

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use toyos_trace::{Entry, Event};

const SELF_PATH: &str = "/system/bin/test_rs_scout_tlb";

const PAGE: usize = 2 << 20;
const LINE: usize = 64;
/// The working sets measured, in 2 MiB pages.
const KS: [usize; 5] = [0, 1, 4, 16, 64];
const WARMUP: usize = 200;
const ROUNDS: usize = 500;
const REPS: usize = 21;

/// `k` pages, each touched at one line, the lines on distinct L1 sets.
struct WorkingSet {
    base: *mut u8,
    layout: Option<std::alloc::Layout>,
    k: usize,
}

impl WorkingSet {
    fn new(k: usize) -> Self {
        if k == 0 {
            return Self { base: core::ptr::null_mut(), layout: None, k };
        }
        let layout = std::alloc::Layout::from_size_align(k * PAGE, PAGE).expect("a layout");
        let base = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!base.is_null(), "{k} pages could not be allocated");
        let ws = Self { base, layout: Some(layout), k };
        ws.touch();
        ws
    }

    #[inline(always)]
    fn touch(&self) {
        for i in 0..self.k {
            let at = i * PAGE + (i % 64) * LINE;
            unsafe {
                let p = self.base.add(at);
                core::ptr::write_volatile(p, core::ptr::read_volatile(p).wrapping_add(1));
            }
        }
    }
}

impl Drop for WorkingSet {
    fn drop(&mut self) {
        if let Some(layout) = self.layout {
            unsafe { std::alloc::dealloc(self.base, layout) };
        }
    }
}

fn child() {
    let have = scout::pmc_programme().is_some();
    let mut input = std::io::stdin().lock();
    let mut out = std::io::stdout().lock();
    let mut ws = WorkingSet::new(0);
    let mut byte = [0u8];
    loop {
        input.read_exact(&mut byte).expect("the parent's next byte");
        match byte[0] {
            b'P' => {
                ws.touch();
                out.write_all(b"p").unwrap();
            }
            b'K' => {
                input.read_exact(&mut byte).expect("the working set's size");
                drop(core::mem::replace(&mut ws, WorkingSet::new(0)));
                ws = WorkingSet::new(byte[0] as usize);
                out.write_all(b"k").unwrap();
            }
            b'M' => {
                let snapshot = scout::pmcs(have);
                let mut bytes = [0u8; 32];
                for (i, v) in snapshot.iter().enumerate() {
                    bytes[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
                }
                out.write_all(&bytes).unwrap();
            }
            b'Q' => return,
            other => panic!("scout_tlb child: unknown command {other:#x}"),
        }
        out.flush().unwrap();
    }
}

struct Peer {
    to: std::process::ChildStdin,
    from: std::process::ChildStdout,
}

impl Peer {
    fn send(&mut self, bytes: &[u8]) {
        self.to.write_all(bytes).expect("write to the child");
    }

    fn expect(&mut self, want: u8) {
        let mut byte = [0u8];
        self.from.read_exact(&mut byte).expect("read from the child");
        assert_eq!(byte[0], want, "the child answered out of turn");
    }

    fn pmcs(&mut self) -> [u64; 4] {
        self.send(b"M");
        let mut bytes = [0u8; 32];
        self.from.read_exact(&mut bytes).expect("the child's counters");
        core::array::from_fn(|i| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap()))
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        return child();
    }
    let programme = scout::pmc_programme();
    let have = programme.is_some();
    println!("scout_tlb: pmc programme {}", programme.unwrap_or("none (counters read as zero)"));

    let mut proc = Command::new(SELF_PATH)
        .arg("child")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the child");
    let me = std::process::id();
    let them = proc.id();
    let mut peer = Peer { to: proc.stdin.take().unwrap(), from: proc.stdout.take().unwrap() };

    for k in KS {
        let ws = WorkingSet::new(k);
        peer.send(&[b'K', k as u8]);
        peer.expect(b'k');
        for _ in 0..WARMUP {
            ws.touch();
            peer.send(b"P");
            peer.expect(b'p');
        }

        let mut per_rt: Vec<u64> = Vec::with_capacity(REPS);
        let mut mine_total = [0u64; 4];
        let mut theirs_total = [0u64; 4];
        let mut mine_cpus = std::collections::BTreeSet::new();
        let mut their_cpus = std::collections::BTreeSet::new();
        let mut lost = 0;
        for _ in 0..REPS {
            let mut cursor = scout::diary_now();
            let theirs_before = peer.pmcs();
            let mine_before = scout::pmcs(have);
            let start = scout::rdtsc();
            for _ in 0..ROUNDS {
                ws.touch();
                peer.send(b"P");
                peer.expect(b'p');
            }
            let end = scout::rdtsc();
            let mine = scout::pmc_delta(scout::pmcs(have), mine_before);
            let theirs = scout::pmc_delta(peer.pmcs(), theirs_before);
            per_rt.push((end - start) / ROUNDS as u64);
            for i in 0..4 {
                mine_total[i] += mine[i];
                theirs_total[i] += theirs[i];
            }
            let (records, gone) = scout::diary(&mut cursor);
            lost += gone;
            for raw in &records {
                let Ok(entry) = Entry::decode(raw) else { continue };
                let (Event::Pick, Some(thread)) = (entry.event, entry.thread) else { continue };
                if thread.pid == me {
                    mine_cpus.insert(entry.cpu);
                } else if thread.pid == them {
                    their_cpus.insert(entry.cpu);
                }
            }
        }
        drop(ws);
        per_rt.sort_unstable();
        let rts = (REPS * ROUNDS) as u64;
        let per = |t: [u64; 4]| -> [String; 4] {
            core::array::from_fn(|i| format!("{}.{:02}", t[i] / rts, t[i] % rts * 100 / rts))
        };
        println!(
            "scout_tlb: k={k} pages rt_cycles min={} p50={} max={} over {REPS}x{ROUNDS} | \
             parent cpus {:?} child cpus {:?} (diary lost {lost}) | parent pmc/rt {:?} | child pmc/rt {:?}",
            per_rt[0],
            per_rt[REPS / 2],
            per_rt[REPS - 1],
            mine_cpus,
            their_cpus,
            per(mine_total),
            per(theirs_total),
        );
    }

    peer.send(b"Q");
    drop(peer);
    assert!(proc.wait().expect("wait the child").success(), "the child failed");
    println!("scout_tlb: tsc {} MHz", scout::tsc_mhz());
}
