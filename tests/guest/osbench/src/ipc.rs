//! A request and its reply between this process and a child serving it, over
//! each system's own IPC: on ToyOS a connection through a port, the native
//! request/reply every service speaks; on Linux a Unix stream socket. **The
//! mechanisms differ by design**, and that difference is what is measured.
//! A ToyOS message past one frame (8 KiB) goes as consecutive frames, which
//! `frames_each_way` counts; a Linux one is one write.
//!
//! The child is this program as `osbench ipc-serve`. Each phase starts with a
//! control message, `[bytes, rounds, mode]` as three little-endian `u32`s;
//! the child then answers `rounds` requests, and a phase of no rounds ends it.

use std::time::Instant;

use crate::measure::{each, io, Samples};
use crate::report::{refuse, result};
use crate::sys::ipc::{self, Peer};
use crate::sys::{self, Counts};
use crate::ns_since;

const ECHO: u32 = 0;
const HANDLE: u32 = 1;

const MECHANISM: &str = if cfg!(target_os = "toyos") { "toyos_connection" } else { "unix_stream_socket" };
const HANDLE_MECHANISM: &str = if cfg!(target_os = "toyos") { "toyos_moved_pipe_end" } else { "scm_rights_then_close" };

fn control(peer: &Peer, bytes: u32, rounds: u32, mode: u32) -> std::io::Result<()> {
    let mut words = [0u8; 12];
    for (slot, word) in words.as_chunks_mut::<4>().0.iter_mut().zip([bytes, rounds, mode]) {
        *slot = word.to_le_bytes();
    }
    peer.send(&words)
}

/// `osbench ipc-serve`: answer every phase its parent asks for.
pub fn serve() {
    let peer = ipc::served().expect("osbench ipc-serve is started by osbench with its end");
    loop {
        let mut words = [0u8; 12];
        peer.recv(&mut words).expect("a control message");
        let word = |i: usize| u32::from_le_bytes(words[i * 4..i * 4 + 4].try_into().expect("four"));
        let (bytes, rounds, mode) = (word(0) as usize, word(1), word(2));
        if rounds == 0 {
            return;
        }
        let mut buf = vec![0u8; bytes];
        for _ in 0..rounds {
            match mode {
                HANDLE => {
                    let token = peer.recv_with(&mut buf).expect("a request carrying a handle");
                    peer.send_with(&buf, token).expect("a reply carrying it back");
                }
                _ => {
                    peer.recv(&mut buf).expect("a request");
                    peer.send(&buf).expect("a reply");
                }
            }
        }
    }
}

pub fn suite() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return refuse("ipc", format!("no path to this program: {e}")),
    };
    let (mut child, peer) = match ipc::start_server(&exe) {
        Ok(pair) => pair,
        Err(e) => return refuse("ipc", format!("the server would not start: {e}")),
    };

    // A request of `bytes` and a reply of as many, round trip: 5,000 after
    // 200 at 8 B and 4 KiB, 2,000 after 100 at 64 KiB.
    for (bytes, warmup, samples) in [(8usize, 200u32, 5000u32), (4096, 200, 5000), (65536, 100, 2000)] {
        let bench = format!("ipc.reqrep.{bytes}");
        let mut buf = vec![0x42u8; bytes];
        let mut run = || -> std::io::Result<Samples<'_>> {
            control(&peer, bytes as u32, warmup + samples, ECHO)?;
            let ran = crate::ran::start();
            for _ in 0..warmup {
                peer.send(&buf)?;
                peer.recv(&mut buf)?;
            }
            crate::clock::settle();
            let before = sys::counts();
            let mut ns = Vec::with_capacity(samples as usize);
            for _ in 0..samples {
                let t = Instant::now();
                peer.send(&buf)?;
                peer.recv(&mut buf)?;
                ns.push(ns_since(t));
            }
            let mut counts = Counts::per_op(&before, u64::from(samples));
            counts.extend(ran.fields());
            Ok((ns, counts))
        };
        match run() {
            Ok((mut ns, counts)) => {
                let mut extra = vec![
                    ("mechanism", MECHANISM.to_string()),
                    ("frames_each_way", ipc::frames(bytes).to_string()),
                ];
                extra.extend(counts);
                result(&bench, "ns", &mut ns, &extra);
            }
            Err(e) => {
                refuse(&bench, e);
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }

    // An 8-byte request carrying one handle, and a reply carrying it back:
    // 5,000 after 200. ToyOS moves the handle; Linux's SCM_RIGHTS installs a
    // copy in the receiver, which closes the one it sent.
    let bench = "ipc.reqrep_handle.8";
    let run = || -> std::io::Result<Samples<'_>> {
        let (warmup, samples) = (200u32, 5000u32);
        control(&peer, 8, warmup + samples, HANDLE)?;
        let mut buf = [0x17u8; 8];
        let mut token = ipc::token()?;
        let mut trip = |token: ipc::Token| -> std::io::Result<ipc::Token> {
            peer.send_with(&buf, token)?;
            peer.recv_with(&mut buf)
        };
        let ran = crate::ran::start();
        for _ in 0..warmup {
            token = trip(token)?;
        }
        crate::clock::settle();
        let before = sys::counts();
        let mut ns = Vec::with_capacity(samples as usize);
        for _ in 0..samples {
            let t = Instant::now();
            token = trip(token)?;
            ns.push(ns_since(t));
        }
        let mut counts = Counts::per_op(&before, u64::from(samples));
        counts.extend(ran.fields());
        Ok((ns, counts))
    };
    match run() {
        Ok((mut ns, counts)) => {
            let mut extra = vec![("mechanism", HANDLE_MECHANISM.to_string())];
            extra.extend(counts);
            result(bench, "ns", &mut ns, &extra);
        }
        Err(e) => refuse(bench, e),
    }
    let _ = control(&peer, 0, 0, ECHO);
    match child.wait() {
        Ok(status) if status.success() => {}
        outcome => refuse("ipc.server", format!("the server ended {outcome:?}")),
    }

    // A connection opened and accepted by this one process, both ends then
    // closed: ToyOS through a port in a namespace this process built, Linux
    // to a listening socket in the abstract namespace. 1,000 after 50.
    match ipc::Rendezvous::new() {
        Ok(rendezvous) => {
            each("ipc.connect_accept", 50, 1000, &[("mechanism", MECHANISM.into())], |_| {
                io("connect", rendezvous.connect_accept())
            })
        }
        Err(e) => refuse("ipc.connect_accept", e),
    }
}
