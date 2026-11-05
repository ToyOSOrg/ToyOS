//! A served file's offset through std, which the client keeps while the
//! description is this process's alone and hands to the server once a child
//! shares it.
//!
//! - A read lands where the seeks before it put the offset, from the start,
//!   from where it was and from the end, and moves it past what it read.
//! - A write moves it past what it wrote, and a file opened to append writes
//!   at the end and leaves the offset there.
//! - A seek before the start or past the largest offset a file has is refused.
//! - A child given the file as its output writes at the parent's offset, and
//!   the parent's next write lands after the child's bytes.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};
use std::os::fd::AsRawFd;

const FILE: &str = "/home/served-file-offset";

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--read-three") => {
            let mut bytes = [0; 3];
            assert_eq!(toyos::files::read(0, &mut bytes).unwrap(), bytes.len());
            std::io::stdout().write_all(&bytes).unwrap();
            return;
        }
        Some("--aliases") => {
            let mut bytes = [0; 3];
            assert_eq!(toyos::files::read(0, &mut bytes).unwrap(), 3);
            assert_eq!(&bytes, b"abc");
            assert_eq!(toyos::files::read(7, &mut bytes).unwrap(), 3);
            assert_eq!(&bytes, b"def");
            toyos::files::seek(7, toyos::files::SeekFrom::Start(1)).unwrap();
            assert_eq!(toyos::files::read(0, &mut bytes[..1]).unwrap(), 1);
            assert_eq!(bytes[0], b'b');
            assert_eq!(toyos::files::write(7, b"Z").unwrap(), 1);
            return;
        }
        _ => {}
    }
    let _ = fs::remove_file(FILE);
    let mut f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(FILE).expect("open");
    f.write_all(b"0123456789").expect("write");
    assert_eq!(f.stream_position().unwrap(), 10, "a write moves the offset past it");

    let mut buf = [0u8; 3];
    assert_eq!(f.seek(SeekFrom::Start(3)).unwrap(), 3);
    f.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"345", "a read from a seek from the start");
    assert_eq!(f.seek(SeekFrom::Current(-2)).unwrap(), 4);
    f.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"456", "a read from a seek from where it was");
    assert_eq!(f.seek(SeekFrom::End(-2)).unwrap(), 8);
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, b"89", "a read from a seek from the end");

    assert_eq!(f.seek(SeekFrom::Current(-11)).unwrap_err().kind(), std::io::ErrorKind::InvalidInput, "before the start");
    assert_eq!(f.stream_position().unwrap(), 10, "a refused seek leaves the offset");
    let past = toyos::fs::MAX_FILE_BYTES + 1;
    assert!(f.seek(SeekFrom::Start(past)).is_err(), "past the largest offset");

    f.seek(SeekFrom::Start(2)).unwrap();
    f.write_all(b"ab").unwrap();
    assert_eq!(f.stream_position().unwrap(), 4);
    let mut appender = OpenOptions::new().append(true).open(FILE).expect("open to append");
    appender.write_all(b"xy").unwrap();
    assert_eq!(appender.stream_position().unwrap(), 12, "an append leaves the offset at the end");
    drop(appender);
    assert_eq!(fs::read(FILE).unwrap(), b"01ab456789xy");
    refused_spawn_keeps_export(f.as_raw_fd());

    // The child writes `CHILD\n` at the parent's offset, 4, and the parent
    // its `!` after it, over the `x`.
    let status = Command::new("/system/bin/toybox")
        .args(["echo", "CHILD"])
        .stdout(Stdio::from(f.try_clone().expect("a second descriptor")))
        .status()
        .expect("spawn echo");
    assert!(status.success(), "echo: {status}");
    f.write_all(b"!").unwrap();
    assert_eq!(fs::read(FILE).unwrap(), b"01abCHILD\n!y", "the child's bytes at the shared offset, then the parent's");
    f.set_len(0).unwrap();
    f.rewind().unwrap();
    f.write_all(b"abcdefghij").unwrap();
    f.rewind().unwrap();
    let own = std::env::args().next().unwrap();
    let mut command = Command::new(&own);
    command.arg("--read-three").stdin(Stdio::from(f.try_clone().unwrap()));
    for expected in [b"abc", b"def"] {
        let out = command.output().expect("repeat a command with the same file input");
        assert!(out.status.success());
        assert_eq!(out.stdout, expected);
    }
    assert_eq!(f.stream_position().unwrap(), 6, "children consume exactly what they read");
    f.read_exact(&mut buf[..2]).unwrap();
    assert_eq!(&buf[..2], b"gh", "parent resumes at the shared offset");
    f.rewind().unwrap();
    posix_aliases(&own, f.as_raw_fd());
    assert_eq!(f.stream_position().unwrap(), 3, "dup2 aliases and parent share one offset");
    assert_eq!(fs::read(FILE).unwrap(), b"abZdefghij");
    fs::remove_file(FILE).unwrap();
    assert_eq!(toyos::files::fstat(f.as_raw_fd()).unwrap().nlink, 0, "an open unlinked file has no cached name count");
    f.rewind().unwrap();
    assert_eq!(f.read(&mut buf).unwrap(), 3);
    assert_eq!(&buf, b"abZ", "open description survives unlink");
    drop(f);
    println!("served_file_offset: passed");
}

fn refused_spawn_keeps_export(fd: i32) {
    use toyos::AsHandle;
    let exported = toyos::files::export(fd).expect("export the shared description");
    let fds = [(1, exported.as_handle(), toyos::fd::Kind::File)];
    let request = toyos::spawn::Spawn {
        program: "/system/bin/toybox", image: None, image_held: false,
        argv: b"toybox\0", env: &[], cwd: "/", fds: &fds,
        endow: Vec::new(), provided: Vec::new(), parent: toyos::spawn::Parent::Caller,
        independent: false, launcher: None, pager: None,
    };
    let toyos::spawn::Start::Helper(helper) = request.start().expect("a helper") else { panic!("a helper became a launch") };
    // No image: the kernel refuses after the SDK duplicated the endowments.
    assert!(matches!(helper.spawn(None), Err(toyos::spawn::Refused::Syscall(toyos_abi::syscall::SyscallError::InvalidArgument))));
    let mut dir = toyos::fs::Dir::adopt(toyos::ipc::Connection::received(exported)).expect("refusal kept the original export");
    let fid = toyos::fs::Fid { id: 1, generation: dir.generation() };
    assert_eq!(dir.seek(fid, toyos::fs::SEEK_CURRENT, 0).unwrap(), 4, "refusal kept the shared offset");
    dir.close(fid);
}

#[repr(C)]
struct FileActions { actions: usize, reserved: [usize; 7] }

#[link(name = "toyos_c", kind = "static")]
unsafe extern "C" {
    fn posix_spawn_file_actions_init(fa: *mut FileActions) -> i32;
    fn posix_spawn_file_actions_adddup2(fa: *mut FileActions, fd: i32, newfd: i32) -> i32;
    fn posix_spawn_file_actions_destroy(fa: *mut FileActions) -> i32;
    fn posix_spawn(pid: *mut i32, path: *const u8, fa: *const FileActions, attr: *const u8, argv: *const *const u8, envp: *const *const u8) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

fn posix_aliases(path: &str, fd: i32) {
    let path = std::ffi::CString::new(path).unwrap();
    let arg = c"--aliases";
    let argv = [path.as_ptr().cast(), arg.as_ptr().cast(), std::ptr::null()];
    let env = [std::ptr::null()];
    let mut actions = FileActions { actions: 0, reserved: [0; 7] };
    let (mut pid, mut status) = (0, 0);
    // SAFETY: initialized file-actions storage and live, terminated C arrays.
    unsafe {
        assert_eq!(posix_spawn_file_actions_init(&mut actions), 0);
        assert_eq!(posix_spawn_file_actions_adddup2(&mut actions, fd, 0), 0);
        assert_eq!(posix_spawn_file_actions_adddup2(&mut actions, 0, 7), 0);
        assert_eq!(posix_spawn(&mut pid, path.as_ptr().cast(), &actions, std::ptr::null(), argv.as_ptr(), env.as_ptr()), 0);
        assert_eq!(posix_spawn_file_actions_destroy(&mut actions), 0);
        assert_eq!(waitpid(pid, &mut status, 0), pid);
    }
    assert_eq!(status, 0, "child checks both dup2 aliases");
}
