//! Measurement only, never lands: `ipc.reqrep`'s exchange with the diary
//! read around each batch, so each round trip's parks, wakes, cross-CPU
//! wakes and the CPUs each side ran on are counted. Prints
//! `note=ipcwake.<bytes>.<what>` lines.

use std::collections::BTreeMap;

use toyos_trace::Event;

use crate::report::{note, refuse};
use crate::sys::ipc;

const BATCH: usize = 50;
const BATCHES: usize = 20;

#[derive(Default)]
struct Tally {
    parks: [u64; 2],
    wakes: [u64; 2],
    cross: [u64; 2],
    picks: [BTreeMap<u16, u64>; 2],
    idles: u64,
    ops: u64,
    ns: Vec<u64>,
}

pub fn suite() {
    let Some(mut diag) = crate::sys::diag::open().filter(|d| d.has_diary()) else { return refuse("ipcwake", "no diary") };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return refuse("ipcwake", format!("no path to this program: {e}")),
    };
    let (mut child, peer) = match ipc::start_server(&exe) {
        Ok(pair) => pair,
        Err(e) => return refuse("ipcwake", format!("the server would not start: {e}")),
    };
    let me = std::process::id();
    let server = child.id();
    let mut lost = 0;
    for bytes in [4096usize, 65536] {
        let rounds = (200 + BATCH * BATCHES) as u32;
        if let Err(e) = crate::ipc::control(&peer, bytes as u32, rounds, 0) {
            return refuse("ipcwake", e);
        }
        let mut buf = vec![0x42u8; bytes];
        for _ in 0..200 {
            if peer.send(&buf).and_then(|()| peer.recv(&mut buf)).is_err() {
                return refuse("ipcwake", "a warmup round failed");
            }
        }
        let mut t = Tally::default();
        for _ in 0..BATCHES {
            let _ = diag.diary();
            for _ in 0..BATCH {
                let s = std::time::Instant::now();
                if peer.send(&buf).and_then(|()| peer.recv(&mut buf)).is_err() {
                    return refuse("ipcwake", "a round failed");
                }
                t.ns.push(s.elapsed().as_nanos() as u64);
            }
            let (entries, l, _) = diag.diary();
            lost += l;
            t.ops += BATCH as u64;
            // The CPU each side's last wake was posted on, to judge its next pick.
            let mut woke_on: BTreeMap<u32, u16> = BTreeMap::new();
            let mut entries: Vec<_> = entries.into_iter().map(|(_, e)| e).collect();
            entries.sort_by_key(|e| e.stamp);
            for e in entries {
                if matches!(e.event, Event::IdleEnter) {
                    t.idles += 1;
                    continue;
                }
                let Some(th) = e.thread else { continue };
                let side = if th.pid == me { 0 } else if th.pid == server { 1 } else { continue };
                match e.event {
                    Event::Park => t.parks[side] += 1,
                    Event::Wake => {
                        t.wakes[side] += 1;
                        woke_on.insert(th.tid | (side as u32) << 31, e.cpu);
                    }
                    Event::Pick => {
                        *t.picks[side].entry(e.cpu).or_default() += 1;
                        if let Some(from) = woke_on.remove(&(th.tid | (side as u32) << 31)) {
                            t.cross[side] += u64::from(from != e.cpu);
                        }
                    }
                    _ => {}
                }
            }
        }
        t.ns.sort_unstable();
        let per = |n: u64| format!("{}.{:02}", n / t.ops, n * 100 / t.ops % 100);
        let cpus = |m: &BTreeMap<u16, u64>| m.iter().map(|(c, n)| format!("{c}:{n}")).collect::<Vec<_>>().join(",");
        note(
            &format!("ipcwake.{bytes}"),
            &[
                ("ops", t.ops.to_string()),
                ("p50_ns", t.ns[t.ns.len() / 2].to_string()),
                ("client_parks", per(t.parks[0])),
                ("server_parks", per(t.parks[1])),
                ("client_wakes", per(t.wakes[0])),
                ("server_wakes", per(t.wakes[1])),
                ("client_cross_cpu_wakes", per(t.cross[0])),
                ("server_cross_cpu_wakes", per(t.cross[1])),
                ("idle_enters", per(t.idles)),
                ("client_cpus", cpus(&t.picks[0])),
                ("server_cpus", cpus(&t.picks[1])),
            ],
        );
    }
    note("ipcwake", &[("lost", lost.to_string())]);
    let _ = crate::ipc::control(&peer, 0, 0, 0);
    let _ = child.wait();
}
