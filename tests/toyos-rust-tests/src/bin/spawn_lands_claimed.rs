//! A spawn whose place is claimed between its commit and its landing lands
//! claimed: the spawn answers its child, the spawner retires it, and the place
//! is published once the child is.
//!
//! That window is the loader's own, so no caller can order a kill inside it:
//! `debug_action::KILL_PLACE_AS_SPAWN_LANDS` has the kernel kill the place of
//! this process's next spawn there. The child parks, so only the retire its
//! spawner posts ends it, and the place's end is published only after the
//! child's: a child claimed and never retired leaves both waits unanswered.
//!
//! Every wait is unbounded: the runner's deadline is the only clock.

use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

use toyos::endow::{self, SVC_LABEL};
use toyos::process::Process;
use toyos::{namespace, port, AsHandle};
use toyos_abi::syscall::{self, debug_action, SpawnArgs, SyscallError};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_spawn_lands_claimed";

/// The name the place's namespace carries the port back to the test under.
const BACK: &str = "back";

/// The place to the test: a copy of its own `self`.
const MSG_SELF: u32 = 1;

/// `process::KILLED_EXIT_CODE`.
const KILLED: i32 = 137;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("place") => place(),
        Some("child") => park(),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    let (acceptor, connector) = port::create().expect("a port of our own");
    let ns = namespace::build().add(BACK, &connector).finish().expect("a namespace carrying the port back");
    let mut place = Command::new(SELF_PATH)
        .arg("place")
        .endow(SVC_LABEL, ns.into_raw().0)
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn the place");
    let conn = acceptor.accept().expect("the place connected back");
    let header = conn.recv_header().expect("the place's word");
    assert_eq!(header.msg_type, MSG_SELF, "the place said something else");
    let [place_self] = conn.recv_handles_exact::<1>().expect("the place sent its self");
    assert!(place.try_wait().expect("ask after the place").is_none(), "the place ended before the marked spawn");

    assert_eq!(
        syscall::debug(debug_action::KILL_PLACE_AS_SPAWN_LANDS),
        0,
        "this kernel does not carry SYS_DEBUG, so nothing kills the place"
    );
    let child = spawn_under(place_self).expect("a spawn past its commit lands, whatever became of its place");
    match spawn_under(place_self) {
        Err(SyscallError::Gone) => {}
        Err(other) => panic!("a second spawn under the place answered {other:?}, not Gone"),
        Ok(_) => panic!("the kernel did not kill the place of the marked spawn"),
    }
    assert_eq!(child.wait(), Ok(KILLED), "the child that landed under a claimed place was not ended, as killed");
    let status = place.wait().expect("wait for the place");
    assert_eq!(status.code(), Some(KILLED), "the place was not ended, as killed");
    syscall::close(place_self);
    println!("spawn_lands_claimed: the child landed claimed and was retired, and its place was published after it");
}

/// `SYS_SPAWN` of this binary as a child under `place`.
fn spawn_under(place: RawHandle) -> Result<Process, SyscallError> {
    let argv = format!("{SELF_PATH}\0child");
    let args = SpawnArgs {
        path_ptr: SELF_PATH.as_ptr() as u64,
        path_len: SELF_PATH.len() as u64,
        argv_ptr: argv.as_ptr() as u64,
        argv_len: argv.len() as u64,
        slot_map_ptr: 0,
        slot_map_count: 0,
        env_ptr: 0,
        env_len: 0,
        endow_ptr: 0,
        endow_count: 0,
        labels_ptr: 0,
        labels_len: 0,
        cwd_ptr: "/".as_ptr() as u64,
        cwd_len: 1,
        image: 0,
        image_len: 0,
        place: u64::from(place.0),
    };
    // SAFETY: every pointer names a buffer of this frame that outlives the call.
    let child = unsafe { syscall::spawn(&args) }?;
    // SAFETY: the kernel installed it for this call.
    Ok(unsafe { Process::from_raw(child) })
}

fn place() -> ! {
    let conn = endow::service(BACK).expect("the test endowed a port back");
    let own = syscall::dup(endow::this_process().as_handle()).expect("a copy of this process's self");
    conn.send_bytes_with_handles(&[own], MSG_SELF, &[]).expect("send the test this process's self");
    park()
}

fn park() -> ! {
    loop {
        std::thread::park();
    }
}
