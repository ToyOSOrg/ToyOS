//! Measurement only, never lands: `ipc.reqrep`'s exchange at 4096 and 65536
//! bytes, each send and receive syscall split by the profile kernel's marks
//! around the message's copy (`scout-ipc-marks`), and this process's copy out
//! of its receive buffer timed. Prints `note=ipccopy.<bytes>.<side>.<syscall>.
//! <from>-<to>` with p10/p50/p90 in nanoseconds and per-frame counts.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use toyos_trace::{Entry, Event};

use crate::report::{note, refuse};
use crate::sys::ipc;

const BATCH: usize = 20;
const BATCHES: usize = 25;

fn mark_name(code: u32) -> &'static str {
    match code {
        1 => "enter",
        2 => "exit",
        8 => "resumed",
        20 => "copy_in",
        21 => "copied_in",
        22 => "copy_out",
        23 => "copied_out",
        _ => "other",
    }
}

fn syscall_name(num: u32) -> &'static str {
    use toyos_abi::syscall::*;
    match u64::from(num) {
        SYS_MESSAGE_SEND => "send",
        SYS_MESSAGE_RECV => "recv",
        _ => "other",
    }
}

fn split(entries: &mut [Entry], me: u32, server: u32, row: &str, segs: &mut BTreeMap<String, Vec<u64>>) {
    entries.sort_by_key(|e| e.stamp);
    let mut open: BTreeMap<(u32, u32), (u32, Vec<(u32, u64)>)> = BTreeMap::new();
    for e in entries.iter() {
        let Event::Mark { index } = e.event else { continue };
        let Some(th) = e.thread else { continue };
        let side = if th.pid == me { "client" } else if th.pid == server { "server" } else { continue };
        let key = (th.pid, th.tid);
        let (code, arg) = (index >> 24, index & 0x00FF_FFFF);
        match code {
            1 => {
                open.insert(key, (arg, vec![(1, e.stamp)]));
            }
            2 => {
                let Some((num, mut marks)) = open.remove(&key) else { continue };
                if num != arg {
                    continue;
                }
                marks.push((2, e.stamp));
                let base = format!("{row}.{side}.{}", syscall_name(num));
                for w in marks.windows(2) {
                    let name = format!("{base}.{}-{}", mark_name(w[0].0), mark_name(w[1].0));
                    segs.entry(name).or_default().push(w[1].1.saturating_sub(w[0].1));
                }
                segs.entry(format!("{base}.total")).or_default().push(e.stamp.saturating_sub(marks[0].1));
            }
            _ => {
                if let Some((_, marks)) = open.get_mut(&key) {
                    marks.push((code, e.stamp));
                }
            }
        }
    }
}

pub fn suite() {
    let Some(mut diag) = crate::sys::diag::open().filter(|d| d.has_diary()) else { return refuse("ipccopy", "no diary") };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return refuse("ipccopy", format!("no path to this program: {e}")),
    };
    let (mut child, peer) = match ipc::start_server(&exe) {
        Ok(pair) => pair,
        Err(e) => return refuse("ipccopy", format!("the server would not start: {e}")),
    };
    let (me, server) = (std::process::id(), child.id());
    let page = toyos_abi::clock::page();
    let ns = |t: u64| toyos_abi::clock::ticks_to_nanos(t, page.period_fs);
    let mut lost = 0;
    for bytes in [4096usize, 65536] {
        if let Err(e) = crate::ipc::control(&peer, bytes as u32, (200 + BATCH * BATCHES) as u32, 0) {
            return refuse("ipccopy", e);
        }
        let mut buf = vec![0x42u8; bytes];
        for _ in 0..200 {
            if peer.send(&buf).and_then(|()| peer.recv(&mut buf)).is_err() {
                return refuse("ipccopy", "a warmup round failed");
            }
        }
        let mut segs = BTreeMap::new();
        let (c0, n0) = (ipc::USER_COPY_NS.load(Ordering::Relaxed), ipc::USER_COPIES.load(Ordering::Relaxed));
        let mut rounds = Vec::new();
        for _ in 0..BATCHES {
            let _ = diag.diary();
            for _ in 0..BATCH {
                let t = std::time::Instant::now();
                if peer.send(&buf).and_then(|()| peer.recv(&mut buf)).is_err() {
                    return refuse("ipccopy", "a round failed");
                }
                rounds.push(t.elapsed().as_nanos() as u64);
            }
            let (entries, l, _) = diag.diary();
            lost += l;
            let mut entries: Vec<Entry> = entries.into_iter().map(|(_, e)| e).collect();
            split(&mut entries, me, server, &bytes.to_string(), &mut segs);
        }
        rounds.sort_unstable();
        let (c1, n1) = (ipc::USER_COPY_NS.load(Ordering::Relaxed), ipc::USER_COPIES.load(Ordering::Relaxed));
        let copies = (n1 - n0).max(1);
        note(
            &format!("ipccopy.{bytes}"),
            &[
                ("rounds", rounds.len().to_string()),
                ("p50_ns", rounds[rounds.len() / 2].to_string()),
                ("client_user_copy_ns_per_frame", ((c1 - c0) / copies).to_string()),
                ("client_frames", copies.to_string()),
            ],
        );
        for (name, mut v) in segs {
            v.sort_unstable();
            let p = |q: usize| ns(v[(v.len() - 1) * q / 100]);
            note(
                &format!("ipccopy.{name}"),
                &[("n", v.len().to_string()), ("p10_ns", p(10).to_string()), ("p50_ns", p(50).to_string()), ("p90_ns", p(90).to_string())],
            );
        }
    }
    note("ipccopy", &[("lost", lost.to_string())]);
    let _ = crate::ipc::control(&peer, 0, 0, 0);
    let _ = child.wait();
}
