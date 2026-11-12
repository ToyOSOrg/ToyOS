//! XSH 2.9.7: writes to one regular file are atomic with respect to each
//! other and to reads. Two threads write whole generations of a served file
//! over each other, one even and one odd, each byte the generation's number
//! mod 256, while a third reads it whole: every read, and the file once both
//! are done, is one generation. Then two threads append records to one file
//! opened to append: every record lands whole, after the end the other left,
//! and none is lost or doubled. On DATA, where holders write against credit
//! and a write that meets another goes to the server, and in /tmp.
//!
//! A guest test because what it holds is the kernel's one write at a time
//! on several CPUs at once, a holder's write against the server's under its
//! hold; the host suites hold each side's rule (`kernel::paged`, the file
//! server's `pages` tests).

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::toyos::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// Thirty-two pages: in one holder's write, and two of the kernel's
/// sixteen-page read copies.
const BYTES: usize = 32 * 4096;
const GENERATIONS: u32 = 200;
const RECORD: usize = 64;
const RECORDS: u32 = 500;

fn main() {
    for dir in ["/home", "/tmp"] {
        let generations = overwrites(&format!("{dir}/a-served-files-writes-land-whole"));
        appends(&format!("{dir}/a-served-files-appends-land-whole"));
        println!("a_served_files_writes_land_whole: {dir}: {generations} generations read, none torn; appends whole");
    }
}

/// Every read and the final file one generation: the number of
/// generations the reader met.
fn overwrites(path: &str) -> usize {
    let _ = fs::remove_file(path);
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path).expect("open");
    assert_eq!(file.write_at(&[0u8; BYTES], 0).expect("the first generation"), BYTES);
    let done = AtomicBool::new(false);
    let seen = thread::scope(|s| {
        let writer = |parity: u32| {
            let file = &file;
            move || {
                for k in 0..GENERATIONS {
                    let byte = (2 * k + parity) as u8;
                    write_whole(file, &[byte; BYTES]);
                }
            }
        };
        let even = s.spawn(writer(0));
        let odd = s.spawn(writer(1));
        let reading = s.spawn(|| {
            let mut buf = vec![0u8; BYTES];
            let mut seen = std::collections::BTreeSet::new();
            while !done.load(Ordering::Acquire) {
                assert_eq!(file.read_at(&mut buf, 0).expect("read"), BYTES, "a short read of a file no write shortens");
                if let Some(at) = buf.iter().position(|&b| b != buf[0]) {
                    panic!("a read is torn: generation {} to byte {at}, then generation {}", buf[0], buf[at]);
                }
                seen.insert(buf[0]);
            }
            seen
        });
        even.join().expect("the even writer");
        odd.join().expect("the odd writer");
        done.store(true, Ordering::Release);
        reading.join().expect("the reader")
    });
    let mut last = vec![0u8; BYTES];
    assert_eq!(file.read_at(&mut last, 0).expect("the last read"), BYTES);
    if let Some(at) = last.iter().position(|&b| b != last[0]) {
        panic!("two writes interleaved: generation {} to byte {at}, then generation {}", last[0], last[at]);
    }
    assert!(seen.len() > 2, "the reader met only generations {seen:?}: nothing ran beside it");
    drop(file);
    fs::remove_file(path).unwrap();
    seen.len()
}

/// `bytes` at offset 0, in as many calls as the system takes them, each
/// call's bytes whole.
fn write_whole(file: &fs::File, bytes: &[u8]) {
    let mut done = 0;
    while done < bytes.len() {
        done += file.write_at(&bytes[done..], done as u64).expect("write");
    }
}

/// Two appenders' records, each whole, all of them, in each writer's order.
fn appends(path: &str) {
    let _ = fs::remove_file(path);
    fs::File::create(path).expect("create");
    thread::scope(|s| {
        for writer in [b'a', b'b'] {
            s.spawn(move || {
                let mut file = OpenOptions::new().append(true).open(path).expect("open to append");
                for k in 0..RECORDS {
                    let mut record = [writer; RECORD];
                    record[..4].copy_from_slice(&k.to_le_bytes());
                    assert_eq!(file.write(&record).expect("append"), RECORD, "an append written short");
                }
            });
        }
    });
    let mut all = Vec::new();
    fs::File::open(path).expect("open to read").read_to_end(&mut all).expect("read back");
    assert_eq!(all.len(), 2 * RECORDS as usize * RECORD, "an append landed on another, or went missing");
    let mut next = [0u32; 2];
    for (at, record) in all.chunks(RECORD).enumerate() {
        let writer = record[RECORD - 1];
        assert!(record[4..].iter().all(|&b| b == writer) && (writer == b'a' || writer == b'b'), "record {at} is torn: {record:?}");
        let k = u32::from_le_bytes(record[..4].try_into().unwrap());
        let slot = usize::from(writer - b'a');
        assert_eq!(k, next[slot], "record {at}: writer {} wrote {k} after {}", writer as char, next[slot]);
        next[slot] += 1;
    }
    fs::remove_file(path).unwrap();
}
