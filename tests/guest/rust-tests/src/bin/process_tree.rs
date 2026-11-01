//! A parent's end takes its children down.
//!
//! **Every end takes the whole subtree.** A starts B; B spawns C, launches D
//! through the supervisor and asks the supervisor for E; B hands A a handle to each and a copy of
//! its own `self`. Then B ends — killed by A, and by a CPU fault, one B per
//! arm — and the arm asserts, once A's wait on B answers:
//!
//! - C and D have ended, as killed;
//! - E, which the supervisor started under its session's owner, runs on: detached is the one way to outlive a starter;
//! - after the kill, a spawn and a launch under B's `self` answer `Gone`.
//!
//! B's first act is a spawn the loader refuses once it is admitted under B: its
//! hold on B goes with it, or B is never published and the arm's wait never
//! answers.
//!
//! **The other arms.** A `MANAGE`-only handle is no place, and the supervisor refuses a
//! launch whose place is a pipe and answers the launches after it. A chain —
//! each link spawns the next directly — stops where the kernel refuses a
//! process more than `MAX_DEPTH` below the supervisor, and dies whole with its
//! first link. And a `cat` a shell `detach`es outlives the shell.
//!
//! This process holds the launcher test-runner endows every job, and hands a
//! duplicate to each child that launches: a direct spawn inherits none.
//!
//! Every wait is unbounded: the harness ceiling is the only clock.

#[path = "../image.rs"]
mod image;

use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};

use toyos::endow::{self, Endowments, FromHandle, SVC_LABEL, SYSCAP_LABEL};
use toyos::ipc::Connection;
use toyos::launch::{self, Launch, Outcome, Parent, LAUNCHER};
use toyos::process::Process;
use toyos::syscap::SysCap;
use toyos::{namespace, port, AsHandle, OwnedHandle};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::{self, SpawnArgs, SyscallError};
use toyos_abi::RawHandle;
use std::os::toyos::process::ExitStatusExt;
use toyos_abi::end::{End, Fault};

const SELF_PATH: &str = "/system/bin/test_rs_process_tree";
/// This program copied to `/tmp`, which the kernel opens itself, under the
/// same name: a chain's links spawn it with no file server's connection
/// each, so a file server's bound on one share's clients is not what stops
/// the chain before its depth does.
const LINK_PATH: &str = "/tmp/test_rs_process_tree";
/// `/system/bin/toybox` under the name of an applet that reads its input until
/// it ends: a declared program, so a launch, and one that runs until killed
/// while A holds its input open.
const HELD: &str = "/system/bin/cat";
const SHELL: &str = "/system/bin/shell";

/// The name B's namespace carries the port back to A under.
const BACK: &str = "back";

/// B to A: the handles to C, D, B's own `self` and E, in that order.
const MSG_GROWN: u32 = 1;
/// A to B: fault.
const MSG_FAULT: u32 = 2;

const KILLED: End = End::Killed;
/// How a write to address 0 ends a process.
const CPU_FAULT: End = End::Fault(Fault::Memory);

/// `toyos_proclife::MAX_DEPTH`, which the kernel refuses a process past.
const MAX_DEPTH: u32 = 64;

/// The links a chain may grow before a refusal must have stopped it: each
/// adds a level, so from any start below the supervisor the refusal comes sooner.
const LINK_BOUND: u32 = MAX_DEPTH + 1;

/// `sched::payload`'s state for a thread whose entry is a zombie, which
/// `sys_sysinfo` also answers for an exited one.
const ZOMBIE: u8 = 3;

#[derive(Clone, Copy, Debug)]
enum Ending {
    /// B waits on A; A kills it.
    Killed,
    CpuFault,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("b") => b(),
        Some("c") => park(),
        Some("link") => link(args[2].parse().expect("a link's index")),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    for end in [Ending::Killed, Ending::CpuFault] {
        an_end_takes_its_subtree(end);
    }
    a_manage_only_handle_is_no_place();
    a_pipe_is_no_place();
    a_chain_stops_at_max_depth_and_dies_whole();
    a_detached_program_outlives_its_shell();
    println!("process_tree: PASS");
}

/// The estate's system capability, for the roster.
fn cap() -> &'static SysCap {
    static CAP: std::sync::OnceLock<SysCap> = std::sync::OnceLock::new();
    CAP.get_or_init(|| {
        Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows every binary it spawns a system capability")
    })
}

/// What A holds of one B and its subtree.
struct Grown {
    b: Child,
    conn: Connection,
    c: Process,
    d: Process,
    b_self: OwnedHandle,
    e: Process,
    /// The input D and E read, kept open so neither ends by itself.
    _held: ChildStdin,
}

/// Start B, holding a port back to A beside this process's own namespace, and
/// take what it sends once its subtree is grown.
fn grow() -> Grown {
    let (acceptor, connector) = port::create().expect("a port of our own");
    let own = endow::namespace().expect("test-runner endows its namespace");
    let ns = namespace::build()
        .keep_all(own)
        .add(BACK, &connector)
        .finish()
        .expect("a namespace with the port back beside ours");
    let mut b = Command::new(SELF_PATH)
        .arg("b")
        .endow(SVC_LABEL, ns.into())
        .endow(LAUNCHER, launcher_copy().into())
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn B");
    let held = b.stdin.take().expect("B's input");
    let conn = acceptor.accept().expect("B connected back");
    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    let frame = rx.recv(&conn).expect("B's word that its subtree is grown");
    assert_eq!(frame.msg_type, MSG_GROWN, "B said something else");
    let [c, d, b_self, e] = rx.take_handles::<4>().expect("B sent four handles");
    let (c, d, e) = (Process::from_handle(c), Process::from_handle(d), Process::from_handle(e));
    Grown { b, conn, c, d, b_self, e, _held: held }
}

fn an_end_takes_its_subtree(end: Ending) {
    let mut grown = grow();
    assert_eq!(grown.c.try_wait(), Err(SyscallError::WouldBlock), "C ended before B did");
    assert_eq!(grown.d.try_wait(), Err(SyscallError::WouldBlock), "D ended before B did");

    let b_code = match end {
        Ending::Killed => {
            grown.b.kill().expect("kill B");
            // Claimed, so admission under it is closed, whether or not its
            // teardown is done.
            under_b_is_gone(&grown);
            KILLED
        }
        Ending::CpuFault => {
            grown.conn.send_bytes(MSG_FAULT, &[]).expect("tell B to fault");
            CPU_FAULT
        }
    };

    let status = grown.b.wait().expect("wait for B");
    assert_eq!(End::from_word(status.into_raw()), Some(b_code), "B ended {end:?} and read {status}");
    assert_eq!(grown.c.try_wait(), Ok(KILLED), "C was not ended, as killed, once B's end was published ({end:?})");
    assert_eq!(grown.d.try_wait(), Ok(KILLED), "D, which the supervisor launched under B, was not ended with B ({end:?})");
    assert_eq!(grown.e.try_wait(), Err(SyscallError::WouldBlock), "E, started under its session's owner, ended with B ({end:?})");

    grown.e.kill().expect("kill E");
    assert_eq!(grown.e.wait(), Ok(KILLED));
    println!("  B {end:?}: C and D ended, as killed; E, under its session's owner, ran on");
}

/// A spawn and a launch placed under B's `self` both answer `Gone`.
fn under_b_is_gone(grown: &Grown) {
    let when = "after its kill";
    match spawn_under(grown.b_self.as_handle().raw()) {
        Err(SyscallError::Gone) => {}
        Err(other) => panic!("a spawn under B {when} answered {other:?}, not Gone"),
        Ok(_) => panic!("a spawn under B {when} started"),
    }
    let place = grown.b_self.try_clone().expect("a copy of B's self to send");
    let conn = launcher();
    let request = Launch {
        program: HELD,
        argv: b"/system/bin/cat",
        env: b"",
        cwd: "/",
        extras: Vec::new(),
        fds: Vec::new(),
        parent: Parent::Place(place),
        job: toyos::job::own().expect("a handle to this process's job").into(),
    };
    match launch::launch(&conn, request) {
        Ok(Outcome::Gone) => {}
        Ok(Outcome::Started(child)) => panic!("a launch under B {when} started {child:?}"),
        Ok(_) => panic!("a launch under B {when} was answered as something other than gone"),
        Err(_) => panic!("the launcher did not answer a launch under B {when}"),
    }
}

/// A connection to the launcher test-runner endowed this process.
fn launcher() -> Connection {
    endow::launcher()
        .expect("test-runner endows every job its launcher")
        .open(LAUNCHER)
        .expect("the launcher answers")
}

/// A duplicate of this process's launcher, for a child that launches.
fn launcher_copy() -> OwnedHandle {
    let held = endow::launcher().expect("test-runner endows every job its launcher");
    held.as_handle().try_clone_to_owned().expect("a copy of the launcher")
}

/// `SYS_SPAWN` of this binary as a C under `place`, killed again if it starts.
fn spawn_under(place: RawHandle) -> Result<Process, SyscallError> {
    let argv = format!("{SELF_PATH}\0c");
    let own = image::image_of(SELF_PATH);
    let (image, image_len) = own.spawn_words();
    let args = SpawnArgs {
        path_ptr: SELF_PATH.as_ptr() as u64,
        path_len: SELF_PATH.len() as u64,
        argv_ptr: argv.as_ptr() as u64,
        argv_len: argv.len() as u64,
        env_ptr: 0,
        env_len: 0,
        endow_ptr: 0,
        endow_count: 0,
        labels_ptr: 0,
        labels_len: 0,
        cwd_ptr: "/".as_ptr() as u64,
        cwd_len: 1,
        image,
        image_len,
        place: u64::from(place.0),
        pager: u64::from(toyos_abi::HANDLE_INVALID.0),
    };
    // SAFETY: every pointer names a buffer of this frame that outlives the call.
    let child = unsafe { syscall::spawn(&args) }?;
    // SAFETY: the kernel installed it for this call.
    Ok(unsafe { Process::from_raw(child) })
}

/// A handle to a live process that carries `MANAGE` and not `WRITE` names no
/// place: the right is refused, and the caller lives.
fn a_manage_only_handle_is_no_place() {
    let mut child = Command::new(SELF_PATH).arg("c").spawn().expect("spawn a C");
    let manage = AsHandle::as_handle(&child).try_clone_narrowed(Rights::MANAGE).expect("a MANAGE-only copy");
    match spawn_under(manage.as_handle().raw()) {
        Err(SyscallError::PermissionDenied) => {}
        Err(other) => panic!("a MANAGE-only place answered {other:?}"),
        Ok(_) => panic!("a MANAGE-only place took a child"),
    }
    drop(manage);
    child.kill().expect("kill the C");
    assert_eq!(End::from_word(child.wait().expect("wait the C").into_raw()), Some(KILLED));
    println!("  a MANAGE-only handle is no place: PermissionDenied");
}

/// A pipe's write end carries `WRITE`, so the kernel reaches the type it is
/// not: the supervisor answers the launch refused, and the arms after this one are the supervisor
/// answering the next.
fn a_pipe_is_no_place() {
    let (_read, write) = toyos::pipe_pair().expect("a pipe of our own");
    let place = write.as_handle().try_clone_to_owned().expect("a duplicate to send");
    let conn = launcher();
    let request = Launch {
        program: HELD,
        argv: b"/system/bin/cat",
        env: b"",
        cwd: "/",
        extras: Vec::new(),
        fds: Vec::new(),
        parent: Parent::Place(place),
        job: toyos::job::own().expect("a handle to this process's job").into(),
    };
    match launch::launch(&conn, request) {
        Ok(Outcome::Refused) => {}
        Ok(Outcome::Started(child)) => panic!("a launch whose place is a pipe started {child:?}"),
        Ok(_) => panic!("a launch whose place is a pipe was answered as something other than refused"),
        Err(_) => panic!("the launcher did not answer a launch whose place is a pipe"),
    }
    println!("  a pipe is no place: the supervisor refused the launch");
}

/// Each link spawns the next: until a spawn is refused for its depth, which
/// the link says on the chain's one output. Killing the first link then ends
/// every one.
fn a_chain_stops_at_max_depth_and_dies_whole() {
    std::fs::copy(SELF_PATH, LINK_PATH).expect("copy this program to /tmp for the chain");
    let mut first = Command::new(LINK_PATH)
        .args(["link", "1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the chain's first link");
    let mut said = BufReader::new(first.stdout.take().expect("the chain's output"));
    let mut started = 0;
    let stop = loop {
        let mut line = String::new();
        assert!(said.read_line(&mut line).expect("read the chain") > 0, "the chain ended without a refusal");
        let line = line.trim().to_string();
        // The refusal is a link's too, so it is read before a link's index.
        if line.contains("refused") {
            break line;
        } else if line.starts_with("unbounded") {
            panic!("the chain passed link {LINK_BOUND} with no depth refusal: {line}");
        } else if let Some(k) = line.strip_prefix("link ") {
            started = k.parse().expect("a link's index");
        }
    };
    assert!(started >= 2, "the chain stopped at link {started}: {stop}");

    first.kill().expect("kill the first link");
    assert_eq!(End::from_word(first.wait().expect("wait the first link").into_raw()), Some(KILLED));
    let live = live_named(&["test_rs_process_tree"]);
    let me = syscall::getpid().raw();
    let others: Vec<_> = live.iter().filter(|(pid, _)| *pid != me).collect();
    assert!(others.is_empty(), "the chain's first link ended and these of it run on: {others:?}");
    println!("  a chain of {started} links stopped at a depth refusal ({stop}) and died whole");
}

fn link(k: u32) -> ! {
    println!("link {k}");
    if k > LINK_BOUND {
        println!("unbounded at link {k}");
        park();
    }
    let next = Command::new(LINK_PATH).args(["link", &(k + 1).to_string()]).spawn();
    if let Err(e) = next {
        println!("link {k}: its next link was refused: {e}");
    }
    park();
}

/// A `cat` a shell `detach`es is its session owner's child, so it runs on once the shell's
/// end is published: it reads the input this process holds open.
fn a_detached_program_outlives_its_shell() {
    let mut shell = Command::new(SHELL)
        .independent()
        .args(["-c", &format!("detach {HELD}")])
        .stdin(Stdio::piped())
        .spawn()
        .expect("run a shell that detaches cat");
    let input = shell.stdin.take().expect("the input the shell and cat read");
    let status = shell.wait().expect("wait the shell");
    assert!(status.success(), "the shell's detach failed: {status:?}");
    let live = live_named(&["cat"]);
    assert_eq!(live.len(), 1, "the cat the shell detached ended with the shell: {live:?}");
    drop(input);
    println!("  a detached cat outlived its shell");
}

/// Every process whose main thread is named one of `names` and has not
/// ended, as `(pid, name)`.
fn live_named(names: &[&str]) -> Vec<(u32, String)> {
    const HEADER: usize = toyos::system::SYSINFO_HEADER_SIZE;
    const ENTRY: usize = toyos::system::SYSINFO_ENTRY_SIZE;
    let mut buf = vec![0u8; HEADER + ENTRY * 1024];
    let n = cap().roster(&mut buf);
    assert!((HEADER..=buf.len()).contains(&n), "sysinfo answered {n}");
    (HEADER..)
        .step_by(ENTRY)
        .take_while(|pos| pos + ENTRY <= n)
        .map(|pos| &buf[pos..pos + ENTRY])
        .filter(|entry| entry[9] == 0 && entry[8] != ZOMBIE)
        .map(|entry| {
            let pid = u32::from_le_bytes(entry[0..4].try_into().unwrap());
            let name = String::from_utf8_lossy(&entry[32..60]).trim_end_matches('\0').to_string();
            (pid, name)
        })
        .filter(|(_, name)| names.contains(&name.as_str()))
        .collect()
}

fn park() -> ! {
    loop {
        std::thread::park();
    }
}

fn b() -> ! {
    let conn = endow::service(BACK).expect("A endowed a port back");
    // Admitted under B, then refused by the loader: its hold on B goes. On
    // `/tmp`, which the kernel opens itself, so its loader is what refuses it.
    assert!(Command::new("/tmp/no_such_program").spawn().is_err(), "a program that is not there started");
    let c = Command::new(SELF_PATH).arg("c").stdin(Stdio::null()).spawn().expect("B spawns C");
    let d = Command::new(HELD).independent().spawn().expect("B launches D");
    let e = Command::new(HELD).independent().detached().spawn().expect("B launches E detached");
    let own = endow::this_process();
    let handles = [
        AsHandle::as_handle(&c).try_clone_to_owned().expect("a copy of C"),
        AsHandle::as_handle(&d).try_clone_to_owned().expect("a copy of D"),
        own.as_handle().try_clone_to_owned().expect("a copy of B's self"),
        AsHandle::as_handle(&e).try_clone_to_owned().expect("a copy of E"),
    ];
    conn.signal_with(MSG_GROWN, handles).expect("send A the subtree");

    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    let frame = rx.recv(&conn).expect("A's word to fault");
    assert_eq!(frame.msg_type, MSG_FAULT, "A said something else");
    // SAFETY: a volatile write to address 0, which no region of this process
    // covers: it faults, and the kernel ends this process before anything
    // after it runs.
    unsafe { core::ptr::null_mut::<u8>().write_volatile(1) };
    panic!("a write to address 0 did not fault");
}
