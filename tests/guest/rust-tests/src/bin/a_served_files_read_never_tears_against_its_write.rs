//! XSH 2.9.7: a read and a write of one regular file are atomic with respect
//! to each other. A writer puts whole generations of a served file, each byte
//! the generation's number mod 256, while a reader on its own description
//! reads the whole file through its paged object; every read is one
//! generation.
//!
//! A guest test because the tear is an interleaving of the kernel's copy of a
//! holder's read with the file server's write into the same frames, on two
//! CPUs; the host suites hold each side's rule (`kernel::paged`, the file
//! server's `pages` tests).

use std::fs::{self, OpenOptions};
use std::os::toyos::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

const FILE: &str = "/home/a-served-files-read-never-tears";
/// Sixty-four pages: four of the kernel's sixteen-page copies, inside one
/// request's window.
const BYTES: usize = 64 * 4096;
const GENERATIONS: u32 = 400;

fn main() {
    let _ = fs::remove_file(FILE);
    let writer = OpenOptions::new().write(true).create(true).truncate(true).open(FILE).expect("open to write");
    assert_eq!(writer.write_at(&[0u8; BYTES], 0).expect("the first generation"), BYTES);
    let reader = OpenOptions::new().read(true).open(FILE).expect("open to read");
    let done = AtomicBool::new(false);

    let (reads, seen) = thread::scope(|s| {
        let reading = s.spawn(|| {
            let mut buf = vec![0u8; BYTES];
            let (mut reads, mut seen) = (0u32, std::collections::BTreeSet::new());
            while !done.load(Ordering::Acquire) {
                assert_eq!(reader.read_at(&mut buf, 0).expect("read"), BYTES, "a short read of a file no write shortens");
                if let Some(at) = buf.iter().position(|&b| b != buf[0]) {
                    panic!("read {reads} is torn: generation {} to byte {at}, then generation {}", buf[0], buf[at]);
                }
                seen.insert(buf[0]);
                reads += 1;
            }
            (reads, seen)
        });
        for generation in 1..=GENERATIONS {
            let byte = generation as u8;
            assert_eq!(writer.write_at(&[byte; BYTES], 0).expect("write"), BYTES, "generation {generation} written short");
        }
        done.store(true, Ordering::Release);
        reading.join().expect("the reader")
    });
    // The premise: the reader ran beside the writes and met more than one.
    assert!(seen.len() > 2, "{reads} reads met only generations {seen:?}: nothing ran beside the writes");
    drop((writer, reader));
    fs::remove_file(FILE).unwrap();
    println!("a_served_files_read_never_tears_against_its_write: {reads} reads, {} generations met, none torn", seen.len());
}
