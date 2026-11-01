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

const FILE: &str = "/home/served-file-offset";

fn main() {
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
    drop(f);
    fs::remove_file(FILE).unwrap();
    println!("served_file_offset: passed");
}
