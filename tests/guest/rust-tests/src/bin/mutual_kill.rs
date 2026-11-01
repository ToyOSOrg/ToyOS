//! Two processes, each holding the other's handle, kill each other at once, and
//! both end with the kernel still up.
//!
//! Each round the parent hands each child a handle to the other and one shared
//! word; both spin on the word, the parent sets it, and both call
//! `SYS_PROCESS_KILL` inside the same few microseconds. Last, a killer handed
//! its own handle kills itself, and ends killed rather than exiting.

use std::io::{Read, Write};
use std::os::toyos::io::AsRawHandle;
use std::os::toyos::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use toyos::ipc::Connection;
use toyos::process::Process;
use toyos::shm::SharedMemory;
use toyos::endow::FromHandle;
use toyos::{endow, namespace, port, OwnedHandle};
use toyos_abi::syscall::{self, SVC_LABEL};
use toyos_abi::RawHandle;
use std::os::toyos::process::ExitStatusExt;
use toyos_abi::end::End;

const SELF_PATH: &str = "/system/bin/test_rs_mutual_kill";

/// The name the child's namespace carries the connector under.
const SERVICE: &str = "mutual";

const ROUNDS: usize = 64;

/// The frame that says the batch — the shared word, then the victim — is queued.
const ARM: u32 = 1;

const WORD_BYTES: usize = 4096;

const KILLED: End = End::Killed;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("killer") => killer(),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    for round in 0..ROUNDS {
        let go = SharedMemory::create(WORD_BYTES).expect("a shared word");
        let (a_conn, mut a) = killer_child();
        let (b_conn, mut b) = killer_child();
        let a_handle = RawHandle(a.as_raw_handle());
        let b_handle = RawHandle(b.as_raw_handle());
        arm(&a_conn, &go, b_handle);
        arm(&b_conn, &go, a_handle);
        armed(&mut a);
        armed(&mut b);

        word(&go).store(1, Ordering::Release);

        let ends = [a.wait().expect("wait a"), b.wait().expect("wait b")].map(|s| End::from_word(s.into_raw()));
        assert!(
            ends.iter().all(|e| *e == Some(KILLED) || *e == Some(End::Exited(0)))
                && ends.contains(&Some(KILLED)),
            "round {round}: two processes that killed each other ended {ends:?}",
        );
    }
    println!("mutual_kill: {ROUNDS} rounds of two processes killing each other, every one ended");

    let go = SharedMemory::create(WORD_BYTES).expect("a shared word");
    let (conn, mut own) = killer_child();
    arm(&conn, &go, RawHandle(own.as_raw_handle()));
    armed(&mut own);
    word(&go).store(1, Ordering::Release);
    let end = End::from_word(own.wait().expect("wait the self-killer").into_raw());
    assert_eq!(end, Some(KILLED), "a process that killed itself ended {end:?}");
    println!("mutual_kill: a process that killed itself ended {KILLED}");
}

/// Spawn a killer holding a connector for a fresh port, and accept it.
fn killer_child() -> (Connection, Child) {
    let (acceptor, connector) = port::create().expect("a port of our own");
    let ns = namespace::build()
        .add(SERVICE, &connector)
        .finish()
        .expect("a namespace carrying one connector");
    let child = Command::new(SELF_PATH)
        .arg("killer")
        .endow(SVC_LABEL, ns.into())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn a killer");
    let conn = acceptor.accept().expect("the killer connected");
    (conn, child)
}

fn arm(conn: &Connection, go: &SharedMemory, victim: RawHandle) {
    // SAFETY: a fresh duplicate, answered for by nothing else.
    let victim = unsafe { OwnedHandle::from_raw(syscall::dup(victim).expect("a handle to send")) };
    conn.signal_with(ARM, [go.share().expect("the word to send"), victim]).expect("send the batch");
}

/// Wait for the killer's one line: it holds its victim and is spinning.
fn armed(child: &mut Child) {
    let mut line = String::new();
    let out = child.stdout.as_mut().expect("killer stdout");
    let mut byte = [0u8; 1];
    while out.read(&mut byte).expect("read the killer's marker") == 1 && byte[0] != b'\n' {
        line.push(byte[0] as char);
    }
    assert_eq!(line, "armed", "the killer never armed");
}

fn word(go: &SharedMemory) -> &AtomicU32 {
    // SAFETY: the region is mapped for as long as `go` lives and 2 MiB-aligned,
    // and every access to this word, here and in the peer, is atomic.
    unsafe { AtomicU32::from_ptr(go.as_ptr().cast()) }
}

fn killer() -> ! {
    let ns = endow::namespace().expect("the parent endowed a namespace");
    let conn = ns.open(SERVICE).expect("connect through the endowed connector");
    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    let frame = rx.recv(&conn).expect("the arming frame");
    assert_eq!(frame.msg_type, ARM, "an unexpected frame");
    let [word_handle, victim] = rx.take_handles::<2>().expect("the word and the victim");
    let go = SharedMemory::lent(word_handle, WORD_BYTES).expect("map the word");
    let victim = Process::from_handle(victim);

    let mut out = std::io::stdout();
    out.write_all(b"armed\n").expect("say armed");
    out.flush().expect("flush");

    while word(&go).load(Ordering::Acquire) == 0 {
        std::hint::spin_loop();
    }
    victim.kill().expect("kill the other killer");
    std::process::exit(0);
}
