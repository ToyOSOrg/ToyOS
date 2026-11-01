//! A round trip between two threads, on one CPU and across two cores, through
//! a pipe and through a futex.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use crate::place;
use crate::report::{refuse, result};
use crate::{ns_since, sys, wake};

const WARMUP: u32 = 500;
const ROUNDS: u32 = 10_000;
/// Every this many rounds both sides read their CPU, outside the time kept,
/// and that round is not kept.
const CHECK_EVERY: u32 = 256;

/// One side of the exchange: `ping` hands the turn over and waits for it
/// back, `pong` waits for it and hands it back.
trait Channel: Send + 'static {
    fn ping(&mut self, round: u32) -> std::io::Result<()>;
    fn pong(&mut self, round: u32) -> std::io::Result<()>;
}

struct PipeEnd {
    reader: std::io::PipeReader,
    writer: std::io::PipeWriter,
}

impl Channel for PipeEnd {
    fn ping(&mut self, _: u32) -> std::io::Result<()> {
        self.writer.write_all(&[1])?;
        self.reader.read_exact(&mut [0])
    }

    fn pong(&mut self, _: u32) -> std::io::Result<()> {
        self.reader.read_exact(&mut [0])?;
        self.writer.write_all(&[1])
    }
}

/// Two words: the round the pinger has handed over and the round the ponger
/// has answered. Each side sleeps on the other's word.
#[derive(Default)]
struct Words {
    ping: AtomicU32,
    pong: AtomicU32,
}

#[derive(Clone)]
struct FutexEnd(Arc<Words>);

fn wait_for(word: &AtomicU32, round: u32) {
    loop {
        let now = word.load(Ordering::Acquire);
        if now == round {
            return;
        }
        sys::futex_wait(word, now);
    }
}

impl Channel for FutexEnd {
    fn ping(&mut self, round: u32) -> std::io::Result<()> {
        self.0.ping.store(round, Ordering::Release);
        sys::futex_wake(&self.0.ping, 1);
        wait_for(&self.0.pong, round);
        Ok(())
    }

    fn pong(&mut self, round: u32) -> std::io::Result<()> {
        wait_for(&self.0.ping, round);
        self.0.pong.store(round, Ordering::Release);
        sys::futex_wake(&self.0.pong, 1);
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Where {
    SameCpu,
    CrossCore,
}

pub fn pipe(same: bool) {
    let pair = || -> std::io::Result<(PipeEnd, PipeEnd)> {
        let (from_ping, to_pong) = std::io::pipe()?;
        let (from_pong, to_ping) = std::io::pipe()?;
        Ok((PipeEnd { reader: from_pong, writer: to_pong }, PipeEnd { reader: from_ping, writer: to_ping }))
    };
    let at = if same { Where::SameCpu } else { Where::CrossCore };
    match pair() {
        Ok((a, b)) => run("pipe", at, a, b),
        Err(e) => refuse(&bench("pipe", at), e),
    }
}

pub fn futex(same: bool) {
    let words = FutexEnd(Arc::default());
    run("futex", if same { Where::SameCpu } else { Where::CrossCore }, words.clone(), words);
}

fn bench(kind: &str, at: Where) -> String {
    let at = match at {
        Where::SameCpu => "same_cpu",
        Where::CrossCore => "cross_core",
    };
    format!("pingpong.{kind}.{at}")
}

/// Round-trip time as the pinger sees it, ns. Both sides are pinned, and
/// `placed_held` counts the checks that found them where they were pinned.
/// Neither side is put on the interrupts' CPU.
fn run(kind: &str, at: Where, mut pinger: impl Channel, mut ponger: impl Channel) {
    let bench = bench(kind, at);
    let ran = crate::ran::start();
    let (go_ping, ping_go) = mpsc::channel::<bool>();
    let (go_pong, pong_go) = mpsc::channel::<bool>();
    let (ping_said, ping_heard) = mpsc::channel();
    let (pong_said, pong_heard) = mpsc::channel();
    let ping_side = place::spawn_on(
        |on| on != wake::IRQ_CPU,
        move || {
            if ping_go.recv() != Ok(true) {
                return;
            }
            let mut kept = Vec::with_capacity(ROUNDS as usize);
            let mut seen = Vec::new();
            for round in 1..=WARMUP + ROUNDS {
                let t = Instant::now();
                if let Err(e) = pinger.ping(round) {
                    let _ = ping_said.send(Err(e.to_string()));
                    return;
                }
                let took = ns_since(t);
                if round % CHECK_EVERY == 0 {
                    seen.push(sys::current_cpu());
                } else if round > WARMUP {
                    kept.push(took);
                }
            }
            let _ = ping_said.send(Ok((kept, seen)));
        },
    );
    let ping_side = match ping_side {
        Ok(placed) => placed,
        Err(why) => return refuse(&bench, format!("the pinger was not placed: {why}")),
    };
    let ping_cpu = ping_side.cpu;
    let wanted = move |on: u32| {
        on != wake::IRQ_CPU
            && match at {
                Where::SameCpu => on == ping_cpu,
                Where::CrossCore => sys::core_of(on) != sys::core_of(ping_cpu),
            }
    };
    let pong_side = place::spawn_on(wanted, move || {
        if pong_go.recv() != Ok(true) {
            return;
        }
        let mut seen = Vec::new();
        for round in 1..=WARMUP + ROUNDS {
            if ponger.pong(round).is_err() {
                break;
            }
            if round % CHECK_EVERY == 0 {
                seen.push(sys::current_cpu());
            }
        }
        let _ = pong_said.send(seen);
    });
    let go = pong_side.is_ok();
    // A refused send is a side that already ended, which its join reports.
    let _ = (go_ping.send(go), go_pong.send(go));
    ping_side.thread.join().expect("the pinger panicked");
    let pong_side = match pong_side {
        Ok(placed) => placed,
        Err(why) => return refuse(&bench, format!("the ponger was not placed: {why}")),
    };
    pong_side.thread.join().expect("the ponger panicked");
    let (mut kept, ping_seen) = match ping_heard.recv() {
        Ok(Ok(measured)) => measured,
        Ok(Err(why)) => return refuse(&bench, why),
        Err(_) => return refuse(&bench, "the pinger said nothing"),
    };
    let pong_seen = pong_heard.recv().unwrap_or_default();
    let checks = ping_seen.len().min(pong_seen.len());
    let held = ping_seen.iter().zip(&pong_seen).filter(|(&ping, &pong)| ping == ping_cpu && wanted(pong)).count();
    let mut fields = vec![
        ("ping_cpu", ping_cpu.to_string()),
        ("pong_cpu", pong_side.cpu.to_string()),
        ("placed_held", format!("{held}/{checks}")),
    ];
    fields.extend(ran.fields());
    result(&bench, "ns", &mut kept, &fields);
}
