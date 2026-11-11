//! A process holds its stream state once, whichever runtime writes: std, the
//! program's own copy of the SDK and libc are three copies of the code in this
//! binary. Stderr is made a log ring of this test's own, and:
//!
//! - a line said in pieces by std, libc and the SDK is one record;
//! - after a redirect asked through libc, std's and the SDK's next lines land
//!   at the new ring;
//! - after a redirect asked through the SDK, libc's next line lands there.
//!
//! It needs the guest because what it tests is which copy of a static a
//! linked image's three runtimes reach, which no host build links.

use std::ptr::NonNull;

use toyos::log::region::{Ring, RING_BYTES};
use toyos::log::ring::Reader;
use toyos::log::stdio::{self, Stream};
use toyos::shm::SharedMemory;

unsafe extern "C" {
    fn dup2(old: i32, new: i32) -> i32;
    fn write(fd: i32, buf: *const u8, len: usize) -> isize;
}

const STDERR: i32 = 2;

/// A ring of this test's own, its reader, and a descriptor naming it.
struct Target {
    region: SharedMemory,
    reader: Reader,
    fd: i32,
}

impl Target {
    fn new() -> Self {
        let region = SharedMemory::create(RING_BYTES).expect("a region for a ring");
        let target = Self { fd: 0, reader: Reader::new(), region };
        target.ring().lay_out();
        let handle = target.region.share().expect("a second handle to the ring");
        let fd = toyos::fd::install(toyos::fd::Object::handle(handle), false, 3).expect("a descriptor for the ring");
        Self { fd, ..target }
    }

    fn ring(&self) -> Ring {
        let base = NonNull::new(self.region.as_ptr()).expect("a mapped region is not null");
        // SAFETY: `region` maps `RING_BYTES` for as long as `self` lives.
        unsafe { Ring::at(base) }
    }

    /// Every record written since the last call, as its text and whether it
    /// ended its line.
    fn said(&mut self) -> Vec<(String, bool)> {
        let ring = self.ring();
        std::iter::from_fn(|| self.reader.next(&ring))
            .map(|body| (String::from_utf8_lossy(body.text()).into_owned(), !body.unended()))
            .collect()
    }
}

fn libc_write(text: &str) {
    // SAFETY: `text` is live for the call.
    let written = unsafe { write(STDERR, text.as_ptr(), text.len()) };
    assert_eq!(written, text.len() as isize, "libc's write of {text:?}");
}

fn main() {
    let kept = toyos::fd::dup(STDERR, 3, false).expect("a second descriptor on this test's stderr");
    let (mut first, mut second) = (Target::new(), Target::new());

    // SAFETY: descriptor 2 is this process's stderr, which `kept` still names.
    unsafe { toyos::fd::dup2(first.fd, STDERR, false) }.expect("the first ring as stderr");
    eprint!("std's piece, ");
    libc_write("libc's piece, ");
    stdio::write(Stream::Err, b"the SDK's piece\n").expect("the SDK's write");
    let joined = first.said();

    // SAFETY: as above; C's `dup2`, of this process's own descriptors.
    assert_eq!(unsafe { dup2(second.fd, STDERR) }, STDERR, "libc's dup2");
    eprintln!("std, after libc's redirect");
    stdio::write(Stream::Err, b"the SDK, after libc's redirect\n").expect("the SDK's write");
    libc_write("libc, after its own redirect\n");
    let (after_c_old, after_c_new) = (first.said(), second.said());

    // SAFETY: as above.
    unsafe { toyos::fd::dup2(first.fd, STDERR, false) }.expect("the first ring as stderr again");
    libc_write("libc, after the SDK's redirect\n");
    let (after_sdk_old, after_sdk_new) = (second.said(), first.said());

    // SAFETY: as above; every line from here goes where this test's lines go.
    unsafe { toyos::fd::dup2(kept, STDERR, false) }.expect("this test's stderr back");

    let line = |text: &str| vec![(text.to_string(), true)];
    assert_eq!(joined, line("std's piece, libc's piece, the SDK's piece"), "three runtimes' pieces of one line");
    assert_eq!(after_c_old, [], "the ring libc's dup2 redirected stderr from");
    assert_eq!(
        after_c_new,
        [line("std, after libc's redirect"), line("the SDK, after libc's redirect"), line("libc, after its own redirect")].concat(),
        "the ring libc's dup2 redirected stderr to"
    );
    assert_eq!(after_sdk_old, [], "the ring the SDK's dup2 redirected stderr from");
    assert_eq!(after_sdk_new, line("libc, after the SDK's redirect"), "the ring the SDK's dup2 redirected stderr to");
    println!("stream_state_is_one: one line of three runtimes' pieces, and each redirect seen by the others");
}
