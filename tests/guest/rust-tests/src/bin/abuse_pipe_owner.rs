//! A number another process published designates nothing here.
//!
//! This test used to sweep `SYS_PIPE_OPEN` over the dense machine-wide `PipeId`
//! space, because a pipe id *was* the authority: mode 0 handed the caller a
//! reader of somebody else's stream, mode 1 a writer into it, and
//! `SYS_SOCKET_CREATE` was the same reach under another name. That family is
//! retired and a pipe end travels as a handle.
//!
//! The property that replaced it is stronger. **A handle is a slot in one
//! process's own table**, so a number lifted out of a sibling's output resolves
//! to this process's slot or to nothing at all — and the attack the old shape
//! describes is not expressible rather than refused.
//!
//! **The sweep cannot survive the fail-fast flip, and what replaces it says
//! more.** Naming a handle a process does not hold is a bug in that process, so
//! the first miss of a 4,000-slot sweep is where the sweeping process ends;
//! there is no run of `NotFound`s to count any more. So each way of naming the
//! victim's ends is a child of its own, and beside them is a child that names a
//! slot *nobody* published — the two die identically, which is the whole claim:
//! the victim's number is not refused here, it is not distinguishable from a
//! number that never meant anything.
//!
//! The victim is what stops all of that being vacuous. It holds a live pipe at
//! the numbers it published for as long as the attacks run, and afterwards
//! reads its own end: nothing the attackers wrote arrived, and its own bytes
//! still cross.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use toyos::AsHandle;
use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::RawHandle;
use std::os::toyos::process::ExitStatusExt;
use toyos_abi::end::{End, Fault};

const SELF_PATH: &str = "/system/bin/test_rs_abuse_pipe_owner";

/// How the kernel ends a process that names a handle it does not hold.
const HANDLE_FAULT: End = End::Fault(Fault::Handle);

/// Where the victim's ends are. High, and nothing in this tree grows a table
/// that far: a process holding 900 handles would be a different bug.
const VICTIM_READ_SLOT: u16 = 900;
const VICTIM_WRITE_SLOT: u16 = 901;

/// The control. One slot above the victim's, published by nobody and held by
/// nothing — so an attacker naming it is doing exactly what an attacker naming
/// the victim's number is doing, and the kernel answers both the same way.
const UNPUBLISHED_SLOT: u32 = 902;

/// Every way there is of reaching a pipe by naming it. Each is a role, and each
/// is aimed at the victim's two numbers.
const REACHES: &[(&str, &str)] = &[
    ("read", "read as if it named something"),
    ("write", "took a write"),
    ("control", "a slot nobody ever published answered"),
];

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("victim") => victim(),
        Some(role) => attacker(role),
        None => test(),
    }
}

fn test() {
    // Pipes this process created are its own business.
    let (own_read, own_write) = toyos::pipe_pair().expect("a pipe of our own");
    own_write.write(b"round trip").expect("write our own pipe");
    let mut buf = [0u8; 16];
    let n = own_read.read(&mut buf).expect("read our own pipe");
    assert_eq!(&buf[..n], b"round trip", "our own pipe did not carry its own bytes");

    // Now the victim. It is a sibling: no shared creator, no IPC connection,
    // and this process holds no handle to its pipe.
    let mut victim = Command::new(SELF_PATH)
        .arg("victim")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn victim");

    let mut out = BufReader::new(victim.stdout.take().expect("victim stdout"));
    let mut line = String::new();
    out.read_line(&mut line).expect("read the victim's handle values");
    let published: Vec<&str> = line.trim().split(' ').collect();
    assert_eq!(published.len(), 2, "the victim publishes both ends");

    for (role, what_would_be_wrong) in REACHES {
        let child = Command::new(SELF_PATH)
            .arg(role)
            .args(&published)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {role}: {e}"));
        let said = child.wait_with_output().unwrap_or_else(|e| panic!("wait {role}: {e}"));
        assert_eq!(
            String::from_utf8_lossy(&said.stdout).trim(),
            format!("reached {role}"),
            "{role} never reached its call",
        );
        assert_eq!(
            End::from_word(said.status.into_raw()),
            Some(HANDLE_FAULT),
            "{role}: {what_would_be_wrong}",
        );
        println!("  {role}: ended the caller, {HANDLE_FAULT}");
    }

    // Release the victim and read what it made of all that. Nothing reached its
    // pipe, and it is still its own.
    drop(victim.stdin.take());
    let mut said = String::new();
    out.read_to_string(&mut said).expect("the victim's report");
    assert!(victim.wait().expect("wait victim").success(), "the victim exited nonzero");
    assert_eq!(
        said.trim(),
        "nothing arrived, and my own bytes did",
        "the victim's pipe did not survive its numbers being presented elsewhere",
    );

    println!("a sibling's handle values are as absent here as a number nobody ever published");
}

/// One way of naming the victim's ends, in a process of its own because the
/// kernel's answer is to end it.
fn attacker(role: &str) -> ! {
    let mut args = std::env::args().skip(2);
    let read = RawHandle(args.next().expect("the victim's read end").parse().expect("a handle"));
    let write = RawHandle(args.next().expect("the victim's write end").parse().expect("a handle"));

    // Printed before the call and flushed, so an arm cannot pass by dying on
    // the way to the thing it is testing.
    println!("reached {role}");
    std::io::stdout().flush().expect("flush the marker");

    let mut buf = [0u8; 16];
    let answered = match role {
        "read" => format!("{:?}", syscall::read_nonblock(read, &mut buf)),
        "write" => format!("{:?}", syscall::write_nonblock(write, b"injected")),
        "control" => {
            format!("{:?}", syscall::read_nonblock(RawHandle(UNPUBLISHED_SLOT), &mut buf))
        }
        other => panic!("unknown role {other:?}"),
    };
    panic!("{role} was answered {answered} instead of ending the caller");
}

/// Holds a live pipe at two numbers it publishes, and afterwards says what
/// crossed it.
fn victim() -> ! {
    // A pipe made once every slot below the victim's is taken lands at the
    // next two fresh ones, and stays there when the fillers go.
    let mut fillers = Vec::new();
    while fillers.last().is_none_or(|h: &toyos::OwnedHandle| h.as_handle().raw().slot() + 1 < VICTIM_READ_SLOT) {
        let filler = toyos::fd::with(1, |o| o.handle().map(|stdout| stdout.try_clone_to_owned()))
            .ok()
            .flatten()
            .expect("the victim is given a standard output")
            .expect("fill the victim's table below its slots");
        fillers.push(filler);
    }
    let (read, write) = toyos::pipe_pair().expect("the pipe the attack is aimed at");
    let (parked_read, parked_write) = (read.as_handle(), write.as_handle());
    assert_eq!((parked_read.raw().slot(), parked_write.raw().slot()), (VICTIM_READ_SLOT, VICTIM_WRITE_SLOT), "the pipe is not where it is published");
    drop(fillers);
    println!("{} {}", parked_read.raw().0, parked_write.raw().0);
    std::io::stdout().flush().expect("flush");

    // Block until the parent closes our stdin, keeping the pipe alive for the
    // length of every attack.
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);

    let mut buf = [0u8; 32];
    assert_eq!(
        syscall::read_nonblock(parked_read.raw(), &mut buf),
        Err(SyscallError::WouldBlock),
        "victim: something was written into the pipe",
    );
    syscall::write_nonblock(parked_write.raw(), b"mine").expect("victim: write its own pipe");
    let n = syscall::read_nonblock(parked_read.raw(), &mut buf).expect("victim: read its own pipe");
    assert_eq!(&buf[..n], b"mine", "victim: its own bytes did not cross");

    println!("nothing arrived, and my own bytes did");
    std::io::stdout().flush().expect("victim: flush the report");
    std::process::exit(0);
}
