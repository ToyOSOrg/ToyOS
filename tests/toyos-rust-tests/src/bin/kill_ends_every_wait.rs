//! A thread killed inside any wait the kernel has for it leaves, and its
//! process ends.
//!
//! One child per wait, each killed once it has said it is about to park and the
//! kernel's roster shows its main thread parked: a futex, a poll ring, a
//! process's end, a thread's end and a sleep. A wait the kill
//! cannot end keeps the child's last thread in its process for ever, and
//! `wait` below never returns; the harness's deadline is what says so.
//! `mutual_kill` holds a kill inside a kill.
//!
//! `posted-poll` is the poll ring's wait a peer keeps from parking: this
//! process's threads write no bytes into the pipe the child watches, so every
//! post sends the child's wait round to look again. A kill those posts hold is
//! held for as long as they win a race and no longer, which the harness's
//! deadline cannot see: the posts stop at a ceiling of their own, [`HELD`],
//! and the child has to have ended before it.

use std::io::{Read, Write};
use std::os::toyos::process::{ChildExt, CommandExt};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use toyos::endow::{Endowments, FromHandle, SYSCAP_LABEL};
use toyos::poller::{Poller, READABLE};
use toyos::process::Process;
use toyos::syscap::SysCap;
use toyos::AsHandle;
use toyos_abi::clock;
use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_kill_ends_every_wait";

/// The label the process-wait child finds the process it waits on under.
const WAITED: &str = "waited";

/// The label the posted-poll child finds the pipe it watches under.
const POSTED: &str = "posted";

/// That pipe's read end, which the child holds until it ends.
struct Posted(RawHandle);

impl FromHandle for Posted {
    unsafe fn from_handle(raw: RawHandle) -> Self {
        Self(raw)
    }
}

/// The threads that post the pipe the posted-poll child watches.
const POSTERS: usize = 4;

/// How long those threads go on posting after the kill before they say the
/// child outlived it: a hang ceiling, for a hang that ends when its peers do.
const HELD: Duration = Duration::from_secs(1);

/// `process::KILLED_EXIT_CODE`.
const KILLED: i32 = 137;

/// `sched::payload::SCHED_BLOCKED`, the state column of the roster.
const BLOCKED: u8 = 2;

/// `sched::payload::SCHED_UNKNOWN`, the state a zombied thread's entry carries.
const ZOMBIE: u8 = 3;

// `sleep` runs before `process-wait` and `thread-join`: both of those arms also
// sleep underneath (the waited process's `nanosleep`, the joined thread's
// `std::thread::sleep`), so a mutation that breaks sleep would otherwise surface
// under one of their names instead of its own.
const WAITS: [&str; 6] = ["futex", "poll", "posted-poll", "sleep", "process-wait", "thread-join"];

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(role) => child(role),
        None => test(),
    }
}

fn test() {
    for role in WAITS {
        // The process the process-wait child waits on, which nothing ends but this.
        let mut waited = (role == "process-wait").then(|| spawn("sleep", None));
        // The pipe the posted-poll child watches, which no byte ever enters.
        let posted = (role == "posted-poll").then(|| syscall::pipe().expect("a pipe to post"));
        let endow = waited
            .as_ref()
            .map(|w| {
                let dup = syscall::dup(RawHandle(w.as_raw_handle())).expect("a handle to endow");
                (WAITED.to_string(), dup.0)
            })
            .or(posted.as_ref().map(|pipe| (POSTED.to_string(), pipe.read.0)));
        let mut child = spawn(role, endow);
        let posts = posted.map(|pipe| Posts::hold(child.id(), pipe.write));
        println!("  {role}: killing");
        child.kill().expect("kill the parked child");
        if let Some(posts) = posts {
            posts.until_the_child_ends();
        }
        let code = child.wait().expect("wait for the killed child").code();
        assert_eq!(code, Some(KILLED), "a child killed in its {role} wait ended with {code:?}");
        if let Some(mut waited) = waited.take() {
            waited.kill().expect("kill the waited process");
            waited.wait().expect("wait for the waited process");
        }
        println!("  {role}: a kill ended it");
    }
    println!("kill_ends_every_wait: every wait a kill reached, it ended");
}

/// Spawn a child in `role`, read its marker, and return once it is parked.
fn spawn(role: &str, endow: Option<(String, u32)>) -> Child {
    let mut command = Command::new(SELF_PATH);
    command.arg(role).stdout(Stdio::piped());
    if let Some((label, handle)) = endow {
        command.endow(&label, handle);
    }
    let mut child = command.spawn().unwrap_or_else(|e| panic!("spawn {role}: {e}"));
    let out = child.stdout.as_mut().expect("child stdout");
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while out.read(&mut byte).expect("read the child's marker") == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    assert_eq!(String::from_utf8_lossy(&line), format!("parked in {role}"), "{role} never reached its wait");
    // The marker says only that the park is next: a kill landing before it ends
    // the child at the marker's own syscall, and the wait the arm names is never
    // killed. Unbounded here: the harness's ceiling is the only clock.
    let pid = child.id();
    assert_ne!(pid, 0, "{role}: the kernel no longer answers for the child");
    println!("  {role}: waiting for the roster to show it parked");
    loop {
        match main_thread_status(pid) {
            RosterStatus::Parked => break,
            RosterStatus::NotParked => std::thread::sleep(Duration::from_millis(10)),
            RosterStatus::Gone => {
                panic!("{role}: the main thread is absent from the roster or zombied before the wait under test parked it")
            }
        }
    }
    child
}

/// The threads posting the pipe the posted-poll child watches.
struct Posts {
    /// Each answers whether the pipe lost its reader before the ceiling.
    threads: Vec<JoinHandle<bool>>,
    /// When the threads stop, in nanoseconds since boot: never, until the kill.
    stop_at: Arc<AtomicU64>,
    write: RawHandle,
}

impl Posts {
    /// Start [`POSTERS`] threads, each writing no bytes into `write` until the
    /// pipe has no reader, and return once the roster shows `pid`'s main
    /// thread out of the park the posts woke it from.
    fn hold(pid: u32, write: RawHandle) -> Self {
        let stop_at = Arc::new(AtomicU64::new(u64::MAX));
        let posting = Arc::new(Barrier::new(POSTERS + 1));
        let threads = (0..POSTERS)
            .map(|_| {
                let (stop_at, posting) = (stop_at.clone(), posting.clone());
                std::thread::spawn(move || {
                    let byte = [0u8];
                    posting.wait();
                    while clock::nanos_since_boot() < stop_at.load(Ordering::Relaxed) {
                        // A slice of a real buffer: the kernel is handed an address it can read.
                        match syscall::write(write, &byte[..0]) {
                            Ok(0) => {}
                            // The child ended, and the pipe's one read end with it.
                            Err(SyscallError::Gone) => return true,
                            other => panic!("a write of no bytes answered {other:?}"),
                        }
                    }
                    false
                })
            })
            .collect();
        // Every thread is posting before the kill: one alone loses the race
        // that holds the child.
        posting.wait();
        println!("  posted-poll: waiting for the roster to show it looking");
        loop {
            match main_thread_status(pid) {
                RosterStatus::NotParked => break,
                RosterStatus::Parked => std::thread::sleep(Duration::from_millis(10)),
                RosterStatus::Gone => panic!("posted-poll: a write of no bytes ended the child's wait"),
            }
        }
        Self { threads, stop_at, write }
    }

    /// After the kill: the posts go on until the child ends, and it has to
    /// end within [`HELD`] of them.
    fn until_the_child_ends(self) {
        self.stop_at.store(clock::nanos_since_boot() + HELD.as_nanos() as u64, Ordering::Relaxed);
        for thread in self.threads {
            assert!(
                thread.join().expect("a posting thread"),
                "posted-poll: the child still watched its pipe {HELD:?} after its kill: the posts held it in its wait"
            );
        }
        syscall::close(self.write);
    }
}

/// What the roster says about `pid`'s main thread.
enum RosterStatus {
    /// In the roster, blocked: parked in the wait under test.
    Parked,
    /// In the roster, not blocked yet.
    NotParked,
    /// Not in the roster, or in it as a zombie: no wait under test can still be ahead of it.
    Gone,
}

/// What the roster says about `pid`'s main thread.
fn main_thread_status(pid: u32) -> RosterStatus {
    const HEADER: usize = toyos::system::SYSINFO_HEADER_SIZE;
    const ENTRY: usize = toyos::system::SYSINFO_ENTRY_SIZE;
    static CAP: OnceLock<SysCap> = OnceLock::new();
    let cap = CAP.get_or_init(|| {
        Endowments::get()
            .take(SYSCAP_LABEL)
            .expect("test-runner endows every binary it spawns a system capability")
    });
    let mut buf = vec![0u8; HEADER + ENTRY * 256];
    let n = cap.roster(&mut buf);
    assert!((HEADER..=buf.len()).contains(&n), "sysinfo answered {n}");
    let header = toyos_abi::syscall::SysinfoHeader::decode(buf[..HEADER].try_into().unwrap());
    assert!(
        header.entries as usize <= 256,
        "the roster holds {} threads, more than the 256-entry buffer this test reads can carry",
        header.entries
    );
    buf[HEADER..n]
        .chunks_exact(ENTRY)
        .find(|entry| u32::from_le_bytes(entry[0..4].try_into().unwrap()) == pid && entry[9] == 0)
        .map_or(RosterStatus::Gone, |entry| match entry[8] {
            BLOCKED => RosterStatus::Parked,
            ZOMBIE => RosterStatus::Gone,
            _ => RosterStatus::NotParked,
        })
}

fn child(role: &str) -> ! {
    match role {
        "futex" => {
            static WORD: AtomicU32 = AtomicU32::new(0);
            say(role);
            // SAFETY: `WORD` is a live, aligned `u32` for the whole program.
            unsafe { syscall::futex_wait(WORD.as_ptr(), 0, None) };
        }
        "poll" => {
            let poller = Poller::new(1);
            say(role);
            poller.wait(1, u64::MAX, |_| {});
        }
        "posted-poll" => {
            let Posted(read) = Endowments::get().take(POSTED).expect("the parent endowed a pipe");
            let poller = Poller::new(Poller::MAX_HANDLES);
            // As many polls as a poller holds, one handle each: a post fires
            // them all, and the wait parks only if no post lands while it
            // looks at every one of them.
            poller.watch_raw(read, READABLE, 0);
            for token in 1..u64::from(Poller::MAX_HANDLES) {
                let dup = syscall::dup(read).expect("another handle to the pipe");
                poller.watch_raw(dup, READABLE, token);
            }
            say(role);
            poller.wait(1, u64::MAX, |_| {});
        }
        "process-wait" => {
            let waited: Process = Endowments::get().take(WAITED).expect("the parent endowed a process");
            say(role);
            let _ = syscall::process_wait(waited.as_handle());
        }
        "thread-join" => {
            let parked = std::thread::spawn(|| loop {
                std::thread::sleep(Duration::from_secs(3600));
            });
            say(role);
            let _ = parked.join();
        }
        "sleep" => {
            say(role);
            syscall::nanosleep(u64::MAX);
        }
        other => panic!("unknown role {other:?}"),
    }
    panic!("{role} came back from a wait nothing ends");
}

fn say(role: &str) {
    let mut out = std::io::stdout();
    out.write_all(format!("parked in {role}\n").as_bytes()).expect("say");
    out.flush().expect("flush");
}
