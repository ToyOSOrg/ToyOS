//! What `posix_spawn` hands the kernel for a child's descriptors, and what
//! `waitpid` answers for each end.

use std::cell::RefCell;

use toyos_abi::end::{End, Fault};

use crate::errcode::{EBADF, EINVAL};
use crate::header::{self, FCNTL_H, SIGNAL_H};
use crate::spawnreq::{any_fd, child_fd, plan, turn, wait_status, Action, Plan, Turn};

const STDIN: u32 = 0x1000;
const STDOUT: u32 = 0x1001;
const STDERR: u32 = 0x1002;
const PIPE: u32 = 0x2003;

/// Every stdio descriptor held and none marked.
const HELD: [Option<(u32, bool)>; 3] = [Some((STDIN, false)), Some((STDOUT, false)), Some((STDERR, false))];

/// This process's descriptor beyond 2 the tests name, holding [`PIPE`].
const PIPE_FD: u32 = 9;

/// A descriptor this process holds but cannot give a child: a stream's, or
/// a socket a library keeps as no kernel handle.
const UNCOPYABLE_FD: u32 = 10;

/// The plan, for this process's `stdio` and its descriptor [`PIPE_FD`], with
/// `open` handing out handles from 0x5000 and recording each close of a
/// handle it opened.
fn planned(stdio: [Option<(u32, bool)>; 3], actions: &[Action<&str>]) -> (Result<Plan<u32>, i32>, Vec<u32>) {
    let next = RefCell::new(0x5000);
    let closed = RefCell::new(Vec::new());
    let held = |fd: u32| match fd {
        0..=2 => Ok(stdio[fd as usize]),
        PIPE_FD => Ok(Some((PIPE, false))),
        UNCOPYABLE_FD => Err(EINVAL),
        _ => Ok(None),
    };
    let plan = plan(
        held,
        actions,
        |path, _, _| match *path {
            "missing" => Err(crate::errcode::ENOENT),
            _ => {
                let handle = *next.borrow();
                *next.borrow_mut() += 1;
                Ok(handle)
            }
        },
        |handle| closed.borrow_mut().push(handle),
    );
    (plan, closed.into_inner())
}

/// A descriptor this process holds but cannot give refuses the spawn with
/// its `errno`, and what the plan opened is closed, rather than the child
/// starting without it.
#[test]
fn a_descriptor_that_cannot_be_given_refuses_the_spawn() {
    let open = Action::Open { fd: 3, path: "/tmp/in", oflag: 0, mode: 0 };
    let (plan, closed) = planned(HELD, &[open, Action::Dup2 { fd: UNCOPYABLE_FD, newfd: 1 }]);
    assert_eq!(plan, Err(EINVAL));
    assert_eq!(closed, [0x5000], "the file the plan opened was left open");
}

/// With no actions, the child holds what this process holds of 0–2 and
/// nothing else.
#[test]
fn a_child_starts_with_stdio_alone() {
    let (plan, _) = planned(HELD, &[]);
    assert_eq!(plan, Ok(Plan { fds: vec![(0, STDIN), (1, STDOUT), (2, STDERR)], opened: vec![] }));
    let (plan, _) = planned([Some((STDIN, false)), None, Some((STDERR, false))], &[]);
    assert_eq!(plan.unwrap().fds, [(0, STDIN), (2, STDERR)], "a stdout this process closed");
}

/// The stage's three: this process's stdout marked `FD_CLOEXEC`, a 0 that
/// `addopen` opened `O_CLOEXEC`, and a 1 that `adddup2` made and `addclose`
/// closed are each absent in the child.
#[test]
fn what_close_on_exec_or_an_addclose_names_is_absent() {
    let cloexec = header::int(FCNTL_H, "O_CLOEXEC");
    let (plan, _) = planned([Some((STDIN, false)), Some((STDOUT, true)), Some((STDERR, false))], &[]);
    assert_eq!(plan.unwrap().fds, [(0, STDIN), (2, STDERR)], "stdout marked FD_CLOEXEC");

    let open = Action::Open { fd: 0, path: "/tmp/in", oflag: cloexec, mode: 0 };
    let (plan, _) = planned(HELD, &[open]);
    let plan = plan.unwrap();
    assert_eq!(plan.fds, [(1, STDOUT), (2, STDERR)], "a 0 opened O_CLOEXEC");
    assert_eq!(plan.opened, [0x5000], "the file opened here is this process's to close");

    let (plan, _) = planned(HELD, &[Action::Dup2 { fd: PIPE_FD, newfd: 1 }, Action::Close { fd: 1 }]);
    assert_eq!(plan.unwrap().fds, [(0, STDIN), (2, STDERR)], "a 1 adddup2 made and addclose closed");
}

/// `adddup2` of a descriptor of this process's beyond 2 is the one way the
/// child holds it, under the number the action gives; one an earlier action
/// closed is `EBADF`, as is a stdio descriptor this process does not hold.
#[test]
fn an_adddup2_names_a_descriptor_for_the_child() {
    let (plan, _) = planned(HELD, &[Action::Dup2 { fd: PIPE_FD, newfd: 1 }]);
    assert_eq!(plan.unwrap().fds, [(0, STDIN), (1, PIPE), (2, STDERR)]);
    let (plan, _) = planned(HELD, &[Action::Dup2 { fd: PIPE_FD, newfd: 7 }]);
    assert_eq!(plan.unwrap().fds, [(0, STDIN), (1, STDOUT), (2, STDERR), (7, PIPE)]);
    let (plan, _) = planned(HELD, &[Action::Close { fd: PIPE_FD }, Action::Dup2 { fd: PIPE_FD, newfd: 1 }]);
    assert_eq!(plan, Err(EBADF), "a descriptor closed in the child");
    let (plan, _) = planned([None, Some((STDOUT, false)), None], &[Action::Dup2 { fd: 2, newfd: 1 }]);
    assert_eq!(plan, Err(EBADF), "a stderr this process does not hold");
}

/// The actions run in order, as in the child: a `dup2` reads what an earlier
/// one left, and onto a descriptor marked close-on-exec it clears the mark.
#[test]
fn the_actions_run_in_order() {
    let actions = [
        Action::Open { fd: 5, path: "/tmp/log", oflag: 1, mode: 0o644 },
        Action::Dup2 { fd: 5, newfd: 2 },
        Action::Close { fd: 5 },
        Action::Dup2 { fd: 2, newfd: 1 },
    ];
    let (plan, _) = planned(HELD, &actions);
    assert_eq!(plan.unwrap().fds, [(0, STDIN), (1, 0x5000), (2, 0x5000)]);
    let (plan, _) = planned([Some((STDIN, false)), Some((STDOUT, true)), None], &[Action::Dup2 { fd: 1, newfd: 1 }]);
    assert_eq!(plan.unwrap().fds, [(0, STDIN), (1, STDOUT)], "dup2 of a descriptor onto itself");
}

/// An open that fails ends the plan, and every file the plan opened before it
/// is closed again here.
#[test]
fn a_failed_open_closes_what_the_plan_opened() {
    let actions = [
        Action::Open { fd: 3, path: "/tmp/a", oflag: 0, mode: 0 },
        Action::Open { fd: 4, path: "missing", oflag: 0, mode: 0 },
    ];
    let (plan, closed) = planned(HELD, &actions);
    assert_eq!(plan, Err(crate::errcode::ENOENT));
    assert_eq!(closed, [0x5000]);
}

/// A descriptor an action makes or reads is one a table holds, and one
/// this process does not hold is `EBADF`.
#[test]
fn a_descriptor_past_a_table_or_not_held_is_ebadf() {
    assert_eq!(child_fd(0), Ok(0));
    assert_eq!(child_fd(4095), Ok(4095));
    for fd in [-1, 4096, i32::MAX] {
        assert_eq!(child_fd(fd), Err(EBADF), "{fd}");
        assert_eq!(any_fd(fd), Err(EBADF), "{fd}");
    }
    let (plan, _) = planned(HELD, &[Action::Dup2 { fd: 8, newfd: 0 }]);
    assert_eq!(plan, Err(EBADF), "a descriptor this process does not hold");
}

/// `<sys/wait.h>`'s macros, as the header spells them.
fn wifexited(s: i32) -> bool {
    s & 0x7f == 0
}
fn wexitstatus(s: i32) -> i32 {
    (s >> 8) & 0xff
}
fn wtermsig(s: i32) -> i32 {
    s & 0x7f
}

/// An exit reads as `WIFEXITED` with its low byte, a kill as `SIGKILL`, and
/// each fault as its `<signal.h>` class's signal: exit 137 is no kill.
#[test]
fn each_end_reads_as_its_wait_status() {
    for code in [0, 3, 137, 255, 256 + 7, -1] {
        let s = wait_status(End::Exited(code));
        assert!(wifexited(s) && wexitstatus(s) == code & 0xff, "exit {code}: {s:#x}");
    }
    let signal = |name| header::int(SIGNAL_H, name);
    for (end, name) in [
        (End::Killed, "SIGKILL"),
        (End::Fault(Fault::Memory), "SIGSEGV"),
        (End::Fault(Fault::Handle), "SIGSEGV"),
        (End::Fault(Fault::Alignment), "SIGBUS"),
        (End::Fault(Fault::Instruction), "SIGILL"),
        (End::Fault(Fault::Arithmetic), "SIGFPE"),
        (End::Fault(Fault::Trap), "SIGTRAP"),
    ] {
        let s = wait_status(end);
        assert!(!wifexited(s) && wtermsig(s) == signal(name), "{end:?}: {s:#x}");
    }
}

/// **With `SIGCHLD` ignored a wait answers no child**: a child that ended is
/// forgotten, and a wait whose every child ended answers `ECHILD`.
#[test]
fn an_ignored_sigchld_answers_no_ended_child() {
    assert_eq!(turn(&[true], true), Turn::Gone);
    assert_eq!(turn(&[true, true], true), Turn::Gone);
    assert_eq!(turn(&[true, false], true), Turn::Watch);
    assert_eq!(turn(&[false], true), Turn::Watch);
    assert_eq!(turn(&[], true), Turn::Gone);
}

/// A wait otherwise answers the first named child that ended.
#[test]
fn a_wait_takes_the_first_ended_child() {
    assert_eq!(turn(&[false, true, true], false), Turn::Take(1));
    assert_eq!(turn(&[false, false], false), Turn::Watch);
    assert_eq!(turn(&[], false), Turn::Gone);
}
