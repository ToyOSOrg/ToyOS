//! A program the supervisor launches holds a null in each stdio descriptor
//! its caller sent nothing for, never the supervisor's own console or log ring:
//! a launched `cat` sent only a stdout reads end of file at once and writes
//! nothing, and a launched `echo` sent no descriptor at all says [`UNSENT`]
//! nowhere. [`SENT`], said by one sent this job's stdout, is what the host
//! waits for before it asserts that.
//!
//! The launches are made through `toyos::spawn` with the descriptors named
//! outright, so which ones a launch carries is this test's.
//!
//! `a_launch_gives_each_unsent_stdio_slot_a_null` runs it on QEMU x86-64
//! (`tests/images/launchcase.toml`), whose test-runner holds a launcher.

use toyos::{AsHandle, BorrowedHandle};
use toyos::spawn::{Parent, Spawn, Start};
use toyos::fd::Kind;

const UNSENT: &str = "nothing-sent-said-this";
const SENT: &str = "slot-sent-said-this";

/// `program` with `argv` launched with exactly `fds`, and waited for.
fn launched(program: &str, argv: &[u8], fds: &[(u32, BorrowedHandle<'_>, Kind)]) {
    let request = Spawn {
        program,
        image: None,
        image_held: false,
        argv,
        env: b"",
        cwd: "/",
        fds,
        endow: Vec::new(),
        provided: Vec::new(),
        parent: Parent::Caller,
        independent: true,
        launcher: None,
        pager: None,
    };
    let child = match request.start().map(|start| match start {
        Start::Launch(launch) => launch.launch(),
        Start::Helper(_) => panic!("an independent spawn of {program} was judged a helper"),
    }) {
        Ok(Ok(child)) => child,
        Ok(Err(refused)) | Err(refused) => panic!("the launch of {program} was refused: {refused:?}"),
    };
    let end = child.wait().unwrap_or_else(|e| panic!("wait for {program}: {e:?}"));
    assert!(end.success(), "{program} launched with descriptors {fds:?} ended {end:?}");
}

fn main() {
    let (read, write) = toyos::pipe_pair().expect("a pipe");
    launched("/system/bin/cat", b"/system/bin/cat", &[(1, write.as_handle(), Kind::Handle)]);
    drop(write);
    let mut buf = [0u8; 64];
    let n = read.read(&mut buf).expect("read what cat wrote");
    assert_eq!(n, 0, "a launched cat sent no stdin wrote {:?}", String::from_utf8_lossy(&buf[..n]));

    launched("/system/bin/echo", format!("/system/bin/echo\0{UNSENT}").as_bytes(), &[]);
    let (out, kind) = toyos::fd::copy(1).expect("this job's stdout").expect("this job holds a stdout");
    launched("/system/bin/echo", format!("/system/bin/echo\0{SENT}").as_bytes(), &[(1, out.as_handle(), kind)]);
    eprintln!("launch_unsent_stdio: a launched cat sent no stdin read end of file");
}
