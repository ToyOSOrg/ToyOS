//! A file server's export: a connection it makes to itself for another
//! process, holding descriptions an open made, which names no path.
//!
//! - The exported connection says hello and is served, and its ids are the
//!   exported descriptions, 1.. in the order named.
//! - A read or a write through either connection moves the one offset both
//!   share, and a seek through one is where the other reads next.
//! - A path on the exported connection is refused `NoDirectory`.
//! - Closing the exporter's id leaves the description to the exported one.

use toyos::fs::{Dir, Error, Refused, O_CREATE, O_READ, O_TRUNCATE, O_WRITE, SEEK_CURRENT, SEEK_START};

const DIR: &str = "fs:/home";
const FILE: &str = "an-export-shares-its-descriptions";

fn main() {
    let names = toyos::endow::namespace().expect("a test is endowed a namespace");
    let mut parent = Dir::connect(names, DIR).expect("the home directory");
    let opened = parent.open(FILE, O_READ | O_WRITE | O_CREATE | O_TRUNCATE).expect("open");
    let (fid, generation) = (opened.fid, opened.generation);
    let written = parent.write(fid, generation, b"abcdef").expect("write");
    assert_eq!((written.len, written.offset), (6, 6));

    let end = parent.export(generation, &[fid]).expect("export");
    let mut child = Dir::adopt(end).expect("the exported connection says hello");
    let theirs = child.generation();
    assert_eq!(child.seek(1, theirs, SEEK_CURRENT, 0), Ok(6), "the export's id 1 has an offset of its own");
    assert_eq!(child.seek(1, theirs, SEEK_START, 2), Ok(2));
    let mut buf = [0u8; 2];
    assert_eq!(parent.read(fid, generation, &mut buf), Ok(2));
    assert_eq!(&buf, b"cd", "the exporter read away from where the export moved the offset");
    assert_eq!(child.read(1, theirs, &mut buf), Ok(2));
    assert_eq!(&buf, b"ef", "the export read away from where the exporter left the offset");
    println!("an_export_shares_its_descriptions: one offset, two connections");

    assert_eq!(child.stat(FILE, true), Err(Refused::Error(Error::NoDirectory)));
    assert!(matches!(child.open(FILE, O_READ), Err(Refused::Error(Error::NoDirectory))));
    println!("an_export_shares_its_descriptions: the export names no path");

    parent.close(fid, generation);
    assert_eq!(child.seek(1, theirs, SEEK_START, 0), Ok(0), "the description went with the exporter's id");
    assert_eq!(child.read(1, theirs, &mut buf), Ok(2));
    assert_eq!(&buf, b"ab");
    child.close(1, theirs);
    parent.unlink(FILE).expect("unlink");
    println!("an_export_shares_its_descriptions: ok");
}
