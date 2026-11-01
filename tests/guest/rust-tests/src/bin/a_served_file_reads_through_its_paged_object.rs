//! A file on DATA read through the kernel: the server answers an open to read
//! with a holder of the file's paged object, and the holder reads what the
//! server wrote without a request.
//!
//! - An open to read carries a holder; an open to write alone, and a
//!   directory, carry none.
//! - The holder reads every byte written through the server, across pages,
//!   stops at the size, and sees a later write and a truncation at once.
//! - A file reopened after its last close reads the same bytes.
//! - std reads the same through its descriptor, positioned and at its offset.
//! - A process started as no pager is refused a pager.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::toyos::fs::FileExt;

use toyos::fs::{Base, Change, Dir, O_CREATE, O_DIRECTORY, O_READ, O_TRUNCATE, O_WRITE};
use toyos::paged::{Pager, Paged};

const DIR: &str = "fs:/home";
const NAME: &str = "a-served-file-reads-through-its-paged-object";
const PATH: &str = "/home/a-served-file-reads-through-its-paged-object";

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(13).wrapping_add(7)).collect()
}

fn read(holder: &Paged, offset: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let n = holder.read(&mut out, offset).expect("a holder's read");
    out.truncate(n);
    out
}

fn main() {
    let names = toyos::endow::namespace().expect("a test is endowed a namespace");
    let mut dir = Dir::connect(names, DIR).expect("the home directory");
    let data = pattern(5 * 4096 + 123);

    let writer = dir.open(Base::Root, NAME, O_WRITE | O_CREATE | O_TRUNCATE).expect("open to write");
    assert!(writer.holder.is_none(), "an open to write alone carries no holder");
    assert_eq!(dir.pwrite(writer.fid, 0, &data[..data.len() / 2]), Ok(data.len() / 2));
    assert_eq!(dir.pwrite(writer.fid, (data.len() / 2) as u64, &data[data.len() / 2..]), Ok(data.len() - data.len() / 2));

    let reader = dir.open(Base::Root, NAME, O_READ).expect("open to read");
    let holder = Paged::received(reader.holder.expect("an open to read carries a holder")).expect("a holder");
    assert_eq!(read(&holder, 0, data.len() + 4096), data, "every byte, and none past the size");
    assert_eq!(read(&holder, 4090, 20), data[4090..4110], "across a page");
    assert_eq!(read(&holder, data.len() as u64, 10), b"", "nothing at the size");

    assert_eq!(dir.pwrite(writer.fid, 3, b"XYZ"), Ok(3));
    assert_eq!(read(&holder, 0, 8), [&data[..3], b"XYZ", &data[6..8]].concat(), "a write through the server, at once");
    let change = Change { size: Some(100), ..Change::default() };
    dir.set_attr(Base::Fid(writer.fid), "", false, &change).expect("truncate");
    assert_eq!(read(&holder, 0, 4096).len(), 100, "a truncation, at once");
    println!("a_served_file_reads_through_its_paged_object: the holder reads what the server wrote");

    dir.close(reader.fid);
    drop(holder);
    dir.close(writer.fid);
    let again = dir.open(Base::Root, NAME, O_READ).expect("open again");
    let holder = Paged::received(again.holder.expect("a holder again")).expect("a holder");
    let want = [&data[..3], b"XYZ", &data[6..100]].concat();
    assert_eq!(read(&holder, 0, 4096), want, "the same bytes after the last close");
    dir.close(again.fid);
    let home = dir.open(Base::Root, "", O_READ | O_DIRECTORY).expect("the directory");
    assert!(home.holder.is_none(), "a directory carries no holder");
    dir.close(home.fid);
    println!("a_served_file_reads_through_its_paged_object: a reopen reads the same, a directory has none");

    let mut f = File::open(PATH).expect("std's open");
    let mut all = Vec::new();
    f.read_to_end(&mut all).expect("std's read");
    assert_eq!(all, want);
    f.seek(SeekFrom::Start(4)).unwrap();
    let mut two = [0u8; 2];
    f.read_exact(&mut two).unwrap();
    assert_eq!(&two, b"YZ", "a read at the offset a seek left");
    let mut at = [0u8; 4];
    assert_eq!(f.read_at(&mut at, 96).unwrap(), 4);
    assert_eq!(at, want[96..100]);
    drop(f);
    println!("a_served_file_reads_through_its_paged_object: std reads the same");

    assert_eq!(
        Pager::create(1, 0, 1, std::time::Duration::from_secs(1)).err(),
        Some(toyos_abi::syscall::SyscallError::PermissionDenied),
        "a process started as no pager made one"
    );
    fs::remove_file(PATH).expect("remove");
    println!("a_served_file_reads_through_its_paged_object: ok");
}
