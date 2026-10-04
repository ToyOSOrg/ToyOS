//! A directory capability is a floor: nothing named through `/home` reaches a
//! file outside it, and a directory this program was not given is refused.
//!
//! - a relative symlink under `/home` whose `..` climbs above it is refused
//!   `PermissionDenied` by the server — refused, not clamped to `/home`, which
//!   would be a different file from the one the link named (the rule
//!   `openat2(2)`'s `RESOLVE_BENEATH` states for Linux);
//! - one that climbs and comes back down inside `/home` is followed;
//! - a client that puts a `..` on the wire itself, past std, is refused
//!   `InvalidArgument` before anything is looked up;
//! - a directory no capability names is nobody's, and nothing under it is
//!   there.

use std::fs;
use std::io::ErrorKind;
use std::os::toyos::fs::symlink;

use toyos::fs::{hello, window_put, Reply, Request, OPEN, O_READ, REPLY, WINDOW_BYTES};
use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos_abi::syscall::SyscallError;

const SECRET: &[u8] = b"/state's own bytes, which nothing under /home may name";
const PLAIN: &[u8] = b"a file under /home, named by a link that climbs and comes back";

fn refused(path: &str, want: ErrorKind) {
    match fs::read(path) {
        Err(e) if e.kind() == want => println!("fs_escape: {path} refused: {e}"),
        Err(e) => panic!("{path}: refused with {e} ({:?}), not {want:?}", e.kind()),
        Ok(bytes) if bytes == SECRET => panic!("{path} read /state's secret through /home"),
        Ok(bytes) => panic!("{path} read {} bytes; it names nothing under /home", bytes.len()),
    }
}

fn fresh_link(target: &str, link: &str) {
    let _ = fs::remove_file(link);
    symlink(target, link).unwrap_or_else(|e| panic!("symlink {link} -> {target}: {e}"));
}

/// A raw client of `fs:/home`, past std's own canonicalisation: `rel` on the
/// wire as it is, and the server's word for it.
fn wire_open(rel: &[u8]) -> Reply {
    let names = toyos::endow::namespace().expect("this program was endowed a namespace");
    let conn = names.open("fs:/home").expect("this program holds fs:/home");
    let window = SharedMemory::create(WINDOW_BYTES).expect("a window");
    hello(&conn, &window).expect("fs:/home answers its hello");
    let answer = |conn: &toyos::ipc::Connection| -> Reply {
        let header = conn.recv_header().expect("a reply");
        assert_eq!(header.msg_type, REPLY, "a reply frame");
        conn.recv_payload(&header).expect("a reply's words")
    };
    // SAFETY: the region is `WINDOW_BYTES` long and outlives this use.
    window_put(unsafe { Window::new(window.as_ptr(), WINDOW_BYTES) }, 0, rel);
    conn.send(OPEN, &Request { len: rel.len() as u64, flags: O_READ, ..Request::new() }).expect("open");
    answer(&conn)
}

fn main() {
    fs::create_dir_all("/state/fs_escape").expect("make /state/fs_escape");
    fs::write("/state/fs_escape/secret", SECRET).expect("write the secret");
    fs::create_dir_all("/home/fs_escape/deeper").expect("make /home/fs_escape/deeper");
    fs::write("/home/fs_escape/plain", PLAIN).expect("write the plain file");

    fresh_link("../state/fs_escape/secret", "/home/fs_escape_out");
    refused("/home/fs_escape_out", ErrorKind::PermissionDenied);
    fresh_link("../../../state/fs_escape/secret", "/home/fs_escape/deeper/out");
    refused("/home/fs_escape/deeper/out", ErrorKind::PermissionDenied);
    // A directory link climbing out, and a name under it.
    fresh_link("../../..", "/home/fs_escape/deeper/up");
    refused("/home/fs_escape/deeper/up/state/fs_escape/secret", ErrorKind::PermissionDenied);

    fresh_link("../plain", "/home/fs_escape/deeper/back");
    let back = fs::read("/home/fs_escape/deeper/back").expect("a link that stays inside /home is followed");
    assert_eq!(back, PLAIN, "the link inside /home names the plain file");

    for rel in [&b"../state/fs_escape/secret"[..], b"fs_escape/../../state/fs_escape/secret", b"/state"] {
        let reply = wire_open(rel);
        assert_eq!(
            reply.status,
            SyscallError::InvalidArgument.to_u64(),
            "{:?} on the wire was answered status {}",
            core::str::from_utf8(rel),
            reply.status
        );
        println!("fs_escape: {:?} on the wire refused", core::str::from_utf8(rel).unwrap_or(""));
    }

    // A directory this program holds no capability for is not a file
    // server's: the name is refused by the namespace, and the path goes to
    // ROOT, which has no such directory.
    assert!(
        toyos::endow::namespace().expect("a namespace").open("fs:/fs_escape_nobodys").is_err(),
        "a capability this program was never given answered"
    );
    refused("/fs_escape_nobodys/secret", ErrorKind::NotFound);

    for link in ["/home/fs_escape_out", "/home/fs_escape/deeper/out", "/home/fs_escape/deeper/up", "/home/fs_escape/deeper/back"] {
        let _ = fs::remove_file(link);
    }
    let _ = fs::remove_dir_all("/home/fs_escape");
    let _ = fs::remove_dir_all("/state/fs_escape");
    println!("fs_escape: PASS");
}
