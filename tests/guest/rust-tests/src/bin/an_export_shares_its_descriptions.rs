//! A file server's export: a connection it makes to itself for another
//! process, holding descriptions an open made, which names no path.
//!
//! - The exported connection says hello and is served, and its ids are the
//!   exported descriptions, 1.. in the order named.
//! - A read or a write through either connection moves the one offset both
//!   share, and a seek through one is where the other reads next.
//! - A path on the exported connection is refused `NoDirectory`.
//! - Closing the exporter's id leaves the description to the exported one.
//! - What a stream was given before a write through an id is written first:
//!   the write lands after every byte of it, and the file ends there.

use toyos::fs::{Base, Dir, Error, Fid, Refused, O_CREATE, O_READ, O_TRUNCATE, O_WRITE, SEEK_CURRENT, SEEK_END, SEEK_START};

const DIR: &str = "fs:/home";
const FILE: &str = "an-export-shares-its-descriptions";
const STREAMED: &str = "an-export-shares-its-descriptions-streamed";

/// What the stream is given: half the pipe's ring, so it is all in the pipe
/// before the write through the id is asked, and the server has drained at
/// most a turn or two of it.
const STREAM_BYTES: usize = 1 << 20;

fn main() {
    let names = toyos::endow::namespace().expect("a test is endowed a namespace");
    let mut parent = Dir::connect(names, DIR).expect("the home directory");
    let fid = parent.open(Base::Root, FILE, O_READ | O_WRITE | O_CREATE | O_TRUNCATE).expect("open").fid;
    let written = parent.write(fid, b"abcdef").expect("write");
    assert_eq!((written.len, written.offset), (6, 6));

    let end = parent.export(&[fid]).expect("export");
    let mut child = Dir::adopt(end).expect("the exported connection says hello");
    let theirs = Fid { id: 1, generation: child.generation() };
    assert_eq!(child.seek(theirs, SEEK_CURRENT, 0), Ok(6), "the export's id 1 has an offset of its own");
    assert_eq!(child.seek(theirs, SEEK_START, 2), Ok(2));
    let mut buf = [0u8; 2];
    assert_eq!(parent.read(fid, &mut buf), Ok(2));
    assert_eq!(&buf, b"cd", "the exporter read away from where the export moved the offset");
    assert_eq!(child.read(theirs, &mut buf), Ok(2));
    assert_eq!(&buf, b"ef", "the export read away from where the exporter left the offset");
    println!("an_export_shares_its_descriptions: one offset, two connections");

    assert_eq!(child.stat(Base::Root, FILE, true), Err(Refused::Error(Error::NoDirectory)));
    assert!(matches!(child.open(Base::Root, FILE, O_READ), Err(Refused::Error(Error::NoDirectory))));
    println!("an_export_shares_its_descriptions: the export names no path");

    parent.close(fid);
    assert_eq!(child.seek(theirs, SEEK_START, 0), Ok(0), "the description went with the exporter's id");
    assert_eq!(child.read(theirs, &mut buf), Ok(2));
    assert_eq!(&buf, b"ab");
    child.close(theirs);
    parent.unlink(Base::Root, FILE, false).expect("unlink");
    println!("an_export_shares_its_descriptions: the description outlived its exporter's id");

    let opened = parent.open(Base::Root, STREAMED, O_READ | O_WRITE | O_CREATE | O_TRUNCATE).expect("open the streamed file");
    let fid = opened.fid;
    let stream = parent.stream(fid).expect("a stream on the description");
    let pattern: Vec<u8> = (0..STREAM_BYTES).map(|i| (i % 251) as u8).collect();
    let mut sent = 0;
    while sent < pattern.len() {
        sent += stream.write(&pattern[sent..]).expect("the stream took its bytes");
    }
    let written = parent.write(fid, b"tail").expect("a write through the id");
    assert_eq!(written.offset, STREAM_BYTES as u64 + 4, "the write did not land after what the stream was given");
    assert_eq!(parent.seek(fid, SEEK_END, 0), Ok(STREAM_BYTES as u64 + 4), "the file does not end at the tail");
    assert_eq!(parent.seek(fid, SEEK_START, STREAM_BYTES as i64 - 2), Ok(STREAM_BYTES as u64 - 2));
    let mut tail = [0u8; 6];
    assert_eq!(parent.read(fid, &mut tail), Ok(6));
    assert_eq!(&tail[..2], &pattern[STREAM_BYTES - 2..], "the stream's last bytes are not before the tail");
    assert_eq!(&tail[2..], b"tail");
    drop(stream);
    parent.close(fid);
    parent.unlink(Base::Root, STREAMED, false).expect("unlink the streamed file");
    println!("an_export_shares_its_descriptions: ok");
}
