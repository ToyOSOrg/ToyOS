//! Measurement scout, never landed: what waking an idle CPU costs, split, on a
//! kernel built with `scout-wake-marks`.
//!
//! Two processes ping-pong an 8-byte message, the sender's counter read just
//! before its write, over a pipe pair. Each side spins `THINK_US` before it
//! answers, so the other's CPU has parked and halted by then, and every
//! message is a wake of a halted CPU by the kick IPI.
//!
//! The split is the diary's: the waker's CPU marks around the ICR write (with
//! a back-to-back pair before it, which prices one record), the target marks
//! the kick handler's first statement and the instruction after `hlt`, and the
//! scheduler's own `Wake`, `Pick`, `TimerArm` records and the mark after a
//! pass's `execute` say the rest. The user-side counters close both ends.
//!
//! Segments, in counter ticks, per direction (waker CPU -> target CPU):
//! - `pre`: waker's user stamp to the kick's first mark (syscall entry, pipe,
//!   wake decision).
//! - `send`: the ICR write, the calibration pair's cost subtracted.
//! - `deliver`: the kick's last mark to the handler's first, across CPUs, so
//!   it carries the two counters' skew; the two directions' mean cancels it.
//! - `exit`: handler entry to the instruction after `hlt`.
//! - `drain`/`pick`: from there to the woken task's `Wake` and `Pick`.
//! - `rearm`: the one-shot's arm, mark to `TimerArm`, calibration subtracted.
//! - `switch`: `Pick` to the woken task's first mark after `execute`.
//! - `post`: that mark to the woken side's user stamp.
//! - `e2e`: user stamp to user stamp, across CPUs.

#[path = "../scout.rs"]
mod scout;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

use toyos_trace::{Entry, Event, Thread};

const SELF_PATH: &str = "/system/bin/test_rs_scout_wake";

const THINK_US: u64 = 200;
const BATCH: usize = 150;
const BATCHES: usize = 10;
const STOP: u64 = u64::MAX;

const CAL_A: u32 = 1;
const CAL_B: u32 = 2;
const KICK_BEGIN: u32 = 3;
const KICK_END: u32 = 4;
const ARRIVE: u32 = 5;
const HALT_RETURN: u32 = 6;
const ARM_BEGIN: u32 = 7;
const STOP_BEGIN: u32 = 8;
const RESUMED: u32 = 9;

fn spin(ticks: u64) {
    let until = scout::rdtsc() + ticks;
    while scout::rdtsc() < until {
        core::hint::spin_loop();
    }
}

fn read_u64(from: &mut impl Read) -> u64 {
    let mut bytes = [0u8; 8];
    from.read_exact(&mut bytes).expect("the peer's word");
    u64::from_le_bytes(bytes)
}

fn child(think: u64) {
    let mut input = std::io::stdin().lock();
    let mut out = std::io::stdout().lock();
    let mut pairs: Vec<(u64, u64)> = Vec::new();
    loop {
        let sent = read_u64(&mut input);
        let got = scout::rdtsc();
        if sent == STOP {
            break;
        }
        pairs.push((sent, got));
        spin(think);
        out.write_all(&scout::rdtsc().to_le_bytes()).unwrap();
        out.flush().unwrap();
    }
    out.write_all(&(pairs.len() as u64).to_le_bytes()).unwrap();
    for (a, b) in pairs {
        out.write_all(&a.to_le_bytes()).unwrap();
        out.write_all(&b.to_le_bytes()).unwrap();
    }
    out.flush().unwrap();
}

fn mark(entry: &Entry) -> Option<(u32, u32)> {
    match entry.event {
        Event::Mark { index } => Some((index >> 24, index & 0x00FF_FFFF)),
        _ => None,
    }
}

fn is_mark(entry: &Entry, code: u32) -> bool {
    mark(entry).is_some_and(|(c, _)| c == code)
}

#[derive(Default)]
struct Split {
    pre: Vec<u64>,
    send: Vec<u64>,
    deliver: Vec<i64>,
    exit: Vec<u64>,
    drain: Vec<u64>,
    pick: Vec<u64>,
    rearm: Vec<u64>,
    switch: Vec<u64>,
    post: Vec<u64>,
    e2e: Vec<u64>,
    cal: Vec<u64>,
}

fn p(v: &mut [u64], q: usize) -> u64 {
    v.sort_unstable();
    scout::percentile(v, q)
}

fn ps(v: &mut [i64], q: usize) -> i64 {
    v.sort_unstable();
    if v.is_empty() {
        return 0;
    }
    v[(v.len() - 1) * q / 100]
}

fn main() {
    let mhz = scout::tsc_mhz();
    let think = THINK_US * mhz;
    if std::env::args().nth(1).as_deref() == Some("child") {
        return child(think);
    }
    let mut proc = Command::new(SELF_PATH)
        .arg("child")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the child");
    let ours = [std::process::id(), proc.id()];
    let mut to = proc.stdin.take().unwrap();
    let mut from = proc.stdout.take().unwrap();

    let mut pairs: Vec<(u64, u64)> = Vec::new();
    let mut records: Vec<Entry> = Vec::new();
    let mut lost = 0;
    let mut undecodable = 0;
    for _ in 0..BATCHES {
        let mut cursor = scout::diary_now();
        for _ in 0..BATCH {
            spin(think);
            to.write_all(&scout::rdtsc().to_le_bytes()).unwrap();
            let sent = read_u64(&mut from);
            pairs.push((sent, scout::rdtsc()));
        }
        let (raw, gone) = scout::diary(&mut cursor);
        lost += gone;
        for r in &raw {
            match Entry::decode(r) {
                Ok(e) => records.push(e),
                Err(_) => undecodable += 1,
            }
        }
    }
    to.write_all(&STOP.to_le_bytes()).unwrap();
    let n = read_u64(&mut from);
    for _ in 0..n {
        let a = read_u64(&mut from);
        let b = read_u64(&mut from);
        pairs.push((a, b));
    }
    drop(to);
    assert!(proc.wait().expect("wait the child").success(), "the child failed");
    pairs.sort_unstable();

    let mut by_cpu: BTreeMap<u16, Vec<Entry>> = BTreeMap::new();
    for e in records {
        by_cpu.entry(e.cpu).or_default().push(e);
    }
    for v in by_cpu.values_mut() {
        v.sort_by_key(|e| e.seq);
        v.dedup_by_key(|e| e.seq);
    }
    let marks_seen = by_cpu.values().flatten().filter(|e| mark(e).is_some()).count();
    println!(
        "scout_wake: {} records over {} cpus, {marks_seen} marks, {lost} lost, {undecodable} undecodable, \
         {} user pairs, tsc {mhz} MHz",
        by_cpu.values().map(Vec::len).sum::<usize>(),
        by_cpu.len(),
        pairs.len(),
    );
    assert!(marks_seen > 0, "no scout mark in the diary: this kernel was not built with scout-wake-marks");

    // Every kick's last mark, by target: (waker cpu, index in its ring, stamp).
    let mut kicks: BTreeMap<u32, Vec<(u16, usize, u64)>> = BTreeMap::new();
    for (&cpu, v) in &by_cpu {
        for (i, e) in v.iter().enumerate() {
            if let Some((KICK_END, target)) = mark(e) {
                kicks.entry(target).or_default().push((cpu, i, e.stamp));
            }
        }
    }
    let window = 50 * mhz;
    let ours = |t: Option<Thread>| t.is_some_and(|t| ours.contains(&t.pid));
    let mut splits: BTreeMap<(u16, u16), Split> = BTreeMap::new();
    let (mut unpaired, mut interleaved, mut busy, mut foreign) = (0, 0, 0, 0);
    // The idle entry's timer stop, wherever it happened: mark to `TimerStop`.
    let mut stop: Vec<u64> = Vec::new();
    let mut cal_all: Vec<u64> = Vec::new();
    for v in by_cpu.values() {
        for (i, e) in v.iter().enumerate() {
            if is_mark(e, STOP_BEGIN) {
                if let Some(ts) = v[i + 1..].iter().take(4).find(|n| n.event == Event::TimerStop) {
                    stop.push(ts.stamp - e.stamp);
                }
            }
            if is_mark(e, CAL_B) && i > 0 && is_mark(&v[i - 1], CAL_A) && e.seq - v[i - 1].seq == 1 {
                cal_all.push(e.stamp - v[i - 1].stamp);
            }
        }
    }
    for (&c, v) in &by_cpu {
        for (i, a) in v.iter().enumerate() {
            if !is_mark(a, ARRIVE) {
                continue;
            }
            let Some(&(w, j, end)) = kicks
                .get(&u32::from(c))
                .and_then(|ks| ks.iter().filter(|k| k.0 != c).min_by_key(|k| k.2.abs_diff(a.stamp)))
                .filter(|k| k.2.abs_diff(a.stamp) < window)
            else {
                unpaired += 1;
                continue;
            };
            let wv = &by_cpu[&w];
            if j < 3
                || !is_mark(&wv[j - 1], KICK_BEGIN)
                || !is_mark(&wv[j - 2], CAL_B)
                || !is_mark(&wv[j - 3], CAL_A)
                || wv[j].seq - wv[j - 3].seq != 3
            {
                interleaved += 1;
                continue;
            }
            let (cal_a, cal_b, begin) = (wv[j - 3].stamp, wv[j - 2].stamp, wv[j - 1].stamp);
            let cal = cal_b - cal_a;
            // The target's records from the arrival to its next idle entry or arrival.
            let tail: Vec<&Entry> = v[i + 1..]
                .iter()
                .take_while(|e| e.event != Event::IdleEnter && !is_mark(e, ARRIVE))
                .collect();
            let Some(h) = tail.iter().position(|e| is_mark(e, HALT_RETURN)) else {
                busy += 1;
                continue;
            };
            let after = &tail[h..];
            let halt = after[0].stamp;
            let Some(wk) = after.iter().find(|e| e.event == Event::Wake && ours(e.thread)) else {
                foreign += 1;
                continue;
            };
            let Some(pk) = after.iter().position(|e| e.event == Event::Pick && e.thread == wk.thread) else {
                foreign += 1;
                continue;
            };
            let Some(ri) = after[pk..].iter().position(|e| is_mark(e, RESUMED) && e.thread == wk.thread) else {
                foreign += 1;
                continue;
            };
            let r = after[pk + ri];
            let s = splits.entry((w, c)).or_default();
            s.cal.push(cal);
            s.send.push((end - begin).saturating_sub(cal));
            s.deliver.push(a.stamp as i64 - end as i64);
            s.exit.push(halt - a.stamp);
            s.drain.push(wk.stamp - halt);
            s.pick.push(after[pk].stamp - halt);
            s.switch.push(r.stamp - after[pk].stamp);
            // `switch_to_current` arms the one-shot after the pick.
            let to_r = pk + ri;
            if let Some(ab) = after[..to_r].iter().position(|e| is_mark(e, ARM_BEGIN)) {
                if let Some(ta) = after[ab..to_r].iter().find(|e| matches!(e.event, Event::TimerArm { .. })) {
                    s.rearm.push((ta.stamp - after[ab].stamp).saturating_sub(cal));
                }
            }
            let at = pairs.partition_point(|&(u0, _)| u0 <= begin);
            if at > 0 {
                let (u0, u1) = pairs[at - 1];
                if begin - u0 < window && u1 >= r.stamp && u1 - r.stamp < window {
                    s.pre.push(begin - u0);
                    s.post.push(u1 - r.stamp);
                    s.e2e.push(u1 - u0);
                }
            }
        }
    }
    println!(
        "scout_wake: arrivals with no kick within 50us {unpaired}, kick marks interleaved {interleaved}, \
         not halted {busy}, not our wake {foreign}"
    );
    let cal50 = p(&mut cal_all, 50);
    println!(
        "scout_wake: one record p50={cal50} ticks over {} pairs; idle-entry timer stop (record included) \
         p10={} p50={} p90={} over {}",
        cal_all.len(),
        p(&mut stop, 10),
        p(&mut stop, 50),
        p(&mut stop, 90),
        stop.len(),
    );

    let ns = |t: i64| t * 1_000 / mhz as i64;
    let mut deliver_p50 = BTreeMap::new();
    for (&(w, c), s) in splits.iter_mut() {
        let line = |name: &str, v: &mut Vec<u64>| {
            format!(
                "{name} p10={} p50={} p90={} ({}ns)",
                p(v, 10),
                p(v, 50),
                p(v, 90),
                ns(p(v, 50) as i64)
            )
        };
        let d50 = ps(&mut s.deliver, 50);
        deliver_p50.insert((w, c), d50);
        println!(
            "scout_wake: cpu{w}->cpu{c} n={} rearm_n={} e2e_n={} | {} | {} | deliver p10={} p50={d50} p90={} | {} | {} | {} | {} | {} | {} | {} | {}",
            s.send.len(),
            s.rearm.len(),
            s.e2e.len(),
            line("cal", &mut s.cal),
            line("send", &mut s.send),
            ps(&mut s.deliver, 10),
            ps(&mut s.deliver, 90),
            line("exit", &mut s.exit),
            line("drain", &mut s.drain),
            line("pick", &mut s.pick),
            line("rearm", &mut s.rearm),
            line("switch", &mut s.switch),
            line("pre", &mut s.pre),
            line("post", &mut s.post),
            line("e2e", &mut s.e2e),
        );
    }
    // The two directions of one pair cancel the counters' skew.
    for (&(w, c), &d) in &deliver_p50 {
        if w < c {
            if let Some(&back) = deliver_p50.get(&(c, w)) {
                let s = splits.get_mut(&(w, c)).unwrap();
                let send = p(&mut s.send, 50) as i64;
                let e2e = p(&mut s.e2e, 50) as i64;
                let b = splits.get_mut(&(c, w)).unwrap();
                let send_b = p(&mut b.send, 50) as i64;
                let e2e_b = p(&mut b.e2e, 50) as i64;
                let deliver = (d + back) / 2;
                let share = |num: i64, den: i64| if den > 0 { num * 1000 / den } else { 0 };
                println!(
                    "scout_wake: cpu{w}<->cpu{c} deliver skew-free {deliver} ticks ({}ns), skew {} ticks; \
                     (send+deliver)/e2e = {} permille and {} permille",
                    ns(deliver),
                    (d - back) / 2,
                    share(send + deliver, e2e),
                    share(send_b + deliver, e2e_b),
                );
            }
        }
    }
    assert!(!splits.is_empty(), "no wake of a halted CPU was paired: the two processes shared a CPU");
    println!("scout_wake: done");
}
