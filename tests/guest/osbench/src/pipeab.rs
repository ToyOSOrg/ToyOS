//! MEASUREMENT ONLY, never lands: the rows pipeperf's fixes are judged by, the
//! same on every arm and no kernel change of its own.
//!
//! - `ab.pipe.natural.<size>`: `system::pipe_throughput`'s round, the reader's
//!   64 KiB buffer where the allocator puts it.
//! - `ab.pipe.<size>.r<r>.w<w>`: the reader's buffer `r` and the writer's chunk
//!   `w` bytes into a page.
//! - `ab.pipe.thread.<size>` and `ab.pipe.child.<size>`: the same writer with
//!   the reader a thread of this process or a process of its own.
//! - `ab.pipe.fill.<size>` and `ab.pipe.drain.<size>`: one thread fills an
//!   empty pipe with 64 KiB and empties it: ns per call, no wait and no wake.
//! - `ab.fs.memory.pread.<len>.d<off>`: positioned cached reads into a buffer
//!   `off` bytes into a page, at page-aligned offsets through a 16 MiB file.
//! - `ab.fs.memory.fresh.<len>` and `ab.fs.memory.shm.<len>`: one cached read
//!   of `len` bytes into memory nothing has touched, anonymous or a fresh
//!   shared-memory region as a spawn's image is read into.
//! - `ab.fs.memory.fresh8m.seq_read`: `persist.seq_read_cached`'s file.
//! - `copyat.place.p<k>`: #1133's 256 KiB `memcpy` placement rows.
//! - `proc.*` and `thread.*`: the `procs` group, whose large spawn reads its
//!   32 MiB image into fresh shared memory.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::File;
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::ns_since;
use crate::report::{note, refuse, result};
use crate::sys::Mapping;

#[cfg(target_os = "linux")]
use std::os::unix::fs::FileExt;
#[cfg(target_os = "toyos")]
use std::os::toyos::fs::FileExt;

const PAGE: usize = 4096;
const MIB: usize = 1 << 20;

/// `len` bytes starting `off` bytes into a page.
struct Aligned {
    base: *mut u8,
    layout: Layout,
    off: usize,
    len: usize,
}

// SAFETY: the block is this value's alone.
unsafe impl Send for Aligned {}

impl Aligned {
    fn new(len: usize, off: usize, byte: u8) -> Self {
        let layout = Layout::from_size_align(len + 2 * PAGE, PAGE).expect("a layout");
        // SAFETY: a non-zero size.
        let base = unsafe { alloc_zeroed(layout) };
        assert!(!base.is_null());
        let mut made = Self { base, layout, off, len };
        made.bytes().fill(byte);
        made
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: inside the block, which this value owns.
        unsafe { std::slice::from_raw_parts_mut(self.base.add(self.off), self.len) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        // SAFETY: allocated with this layout.
        unsafe { dealloc(self.base, self.layout) };
    }
}

/// Where the reader's buffer or the writer's chunk lies.
#[derive(Clone, Copy)]
enum Place {
    /// A `Vec`, wherever the allocator puts it.
    Natural,
    /// This far into a page.
    At(usize),
}

enum Buf {
    Vec(Vec<u8>),
    Aligned(Aligned),
}

impl Buf {
    fn new(place: Place, len: usize, byte: u8) -> Self {
        match place {
            Place::Natural => Buf::Vec(vec![byte; len]),
            Place::At(off) => Buf::Aligned(Aligned::new(len, off, byte)),
        }
    }

    fn bytes(&mut self) -> &mut [u8] {
        match self {
            Buf::Vec(v) => v,
            Buf::Aligned(a) => a.bytes(),
        }
    }
}

struct Round {
    mbps: u64,
    reads: u64,
    bytes: u64,
    writes: u64,
    reader_at: usize,
    writer_at: usize,
}

/// The writer's side of a round: 64 writes at a time for 100 ms.
fn write_for(tx: &mut impl Write, chunk: &[u8]) -> std::io::Result<u64> {
    let start = Instant::now();
    let mut writes = 0u64;
    while start.elapsed() < Duration::from_millis(100) {
        for _ in 0..64 {
            tx.write_all(chunk)?;
        }
        writes += 64;
    }
    Ok(writes)
}

/// The reader's side of a round: `(bytes, reads, ns)` from its first read on.
fn read_all(rx: &mut impl Read, buf: &mut [u8]) -> std::io::Result<(u64, u64, u64)> {
    if rx.read(buf)? == 0 {
        return Err(std::io::Error::other("the writer ended before writing"));
    }
    let (mut bytes, mut reads, start) = (0u64, 0u64, Instant::now());
    loop {
        match rx.read(buf)? {
            0 => break,
            n => {
                bytes += n as u64;
                reads += 1;
            }
        }
    }
    Ok((bytes, reads, ns_since(start).max(1)))
}

/// `system::pipe_throughput`'s round, the reader a thread of this process.
fn round(size: usize, reader: Place, writer: Place) -> std::io::Result<Round> {
    let (mut rx, mut tx) = std::io::pipe()?;
    let writing = thread::spawn(move || -> std::io::Result<(u64, usize)> {
        let mut chunk = Buf::new(writer, size, 0x5a);
        let chunk = chunk.bytes();
        Ok((write_for(&mut tx, chunk)?, chunk.as_ptr() as usize % PAGE))
    });
    let mut buf = Buf::new(reader, 64 * 1024, 0);
    let buf = buf.bytes();
    let (bytes, reads, ns) = read_all(&mut rx, buf)?;
    let (writes, writer_at) = writing.join().expect("the writer panicked")?;
    Ok(Round { mbps: bytes * 1000 / ns, reads, bytes, writes, reader_at: buf.as_ptr() as usize % PAGE, writer_at })
}

/// The reader in a child process of its own (`pipe-reader <offset>`): its
/// stdin read into a buffer `offset` bytes into a page until the end, then
/// `bytes reads ns` printed.
pub fn child_reader(args: &[String]) {
    let at: usize = args.first().and_then(|a| a.parse().ok()).expect("pipe-reader <offset>");
    let mut buf = Aligned::new(64 * 1024, at, 0);
    let (bytes, reads, ns) = read_all(&mut std::io::stdin().lock(), buf.bytes()).expect("the reader's reads");
    println!("{bytes} {reads} {ns}");
}

/// One round with the reader in a child process.
fn child_round(size: usize, at: usize) -> std::io::Result<Round> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe).args(["pipe-reader", &at.to_string()]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
    let mut tx = child.stdin.take().expect("a piped stdin");
    let mut chunk = Aligned::new(size, at, 0x5a);
    let writes = write_for(&mut tx, chunk.bytes())?;
    drop(tx);
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!("the reader ended {}", out.status)));
    }
    let said = String::from_utf8_lossy(&out.stdout);
    let v: Vec<u64> = said.split_whitespace().filter_map(|w| w.parse().ok()).collect();
    let [bytes, reads, ns] = v[..] else { return Err(std::io::Error::other(format!("the reader said {said:?}"))) };
    Ok(Round { mbps: bytes * 1000 / ns.max(1), reads, bytes, writes, reader_at: at, writer_at: at })
}

fn report(bench: &str, size: usize, rounds: &[Round]) {
    let writes: u64 = rounds.iter().map(|r| r.writes).sum();
    let reads: u64 = rounds.iter().map(|r| r.reads).sum();
    let bytes: u64 = rounds.iter().map(|r| r.bytes).sum();
    let fields = [
        ("write_bytes", size.to_string()),
        ("reads", reads.to_string()),
        ("writes", writes.to_string()),
        ("bytes_per_read", (bytes / reads.max(1)).to_string()),
        ("reader_at", rounds[0].reader_at.to_string()),
        ("writer_at", rounds[0].writer_at.to_string()),
    ];
    let mut mbps: Vec<u64> = rounds.iter().map(|r| r.mbps).collect();
    result(bench, "MB/s", &mut mbps, &fields);
}

/// One warm-up round, then `rounds` measured.
fn rounds(bench: &str, size: usize, n: usize, one: impl Fn() -> std::io::Result<Round>) {
    let run = || -> std::io::Result<Vec<Round>> {
        one()?;
        crate::clock::settle();
        (0..n).map(|_| one()).collect()
    };
    match run() {
        Ok(measured) => report(bench, size, &measured),
        Err(e) => refuse(bench, e),
    }
}

/// One thread fills an empty pipe with 64 KiB in `size`-byte writes and
/// empties it in `size`-byte reads: ns per write and per read.
fn fill_drain(size: usize, off: usize) {
    const TOTAL: usize = 64 * 1024;
    let calls = TOTAL / size;
    let suffix = if off == 0 { String::new() } else { format!(".d{off}") };
    let (wname, rname) = (format!("ab.pipe.fill.{size}{suffix}"), format!("ab.pipe.drain.{size}{suffix}"));
    let run = |passes: usize| -> std::io::Result<(Vec<u64>, Vec<u64>)> {
        let (mut rx, mut tx) = std::io::pipe()?;
        let mut chunk = Aligned::new(size, off, 0x5a);
        let mut buf = Aligned::new(size, off, 0);
        let (mut w, mut r) = (Vec::new(), Vec::new());
        for _ in 0..passes {
            let t = Instant::now();
            for _ in 0..calls {
                tx.write_all(chunk.bytes())?;
            }
            w.push(ns_since(t) / calls as u64);
            let t = Instant::now();
            for _ in 0..calls {
                rx.read_exact(buf.bytes())?;
            }
            r.push(ns_since(t) / calls as u64);
        }
        Ok((w, r))
    };
    let fields = [("call_bytes", size.to_string()), ("at", off.to_string())];
    let measured = run(20).and_then(|_| {
        crate::clock::settle();
        run(300)
    });
    match measured {
        Ok((mut w, mut r)) => {
            result(&wname, "ns", &mut w, &fields);
            result(&rname, "ns", &mut r, &fields);
        }
        Err(e) => refuse(&wname, e),
    }
}

fn pattern(path: &std::path::Path, len: usize) -> std::io::Result<()> {
    let block: Vec<u8> = (0..MIB).map(|i| (i % 251) as u8).collect();
    let mut f = File::create(path)?;
    for _ in 0..len / MIB {
        f.write_all(&block)?;
    }
    Ok(())
}

const FILE: usize = 16 * MIB;

/// Where the `i`th read of `step` bytes starts: page-aligned, in order
/// through the file.
fn nth(i: usize, step: usize) -> u64 {
    (i * step % (FILE - step + PAGE) / PAGE * PAGE) as u64
}

/// Positioned reads of `len` bytes into a buffer `off` bytes into a page: ns
/// per read.
fn pread(f: &File, len: usize, off: usize, samples: usize) {
    let bench = format!("ab.fs.memory.pread.{len}.d{off}");
    let mut buf = Aligned::new(len, off, 0);
    let step = len.max(PAGE);
    let mut pass = |n: usize, keep: &mut Vec<u64>| -> std::io::Result<()> {
        for i in 0..n {
            let t = Instant::now();
            let got = f.read_at(buf.bytes(), nth(i, step))?;
            keep.push(ns_since(t));
            if got != len {
                return Err(std::io::Error::other(format!("read {got} of {len}")));
            }
        }
        Ok(())
    };
    let mut ns = Vec::new();
    let run = pass(samples / 10 + 1, &mut Vec::new()).and_then(|()| {
        crate::clock::settle();
        pass(samples, &mut ns)
    });
    match run {
        Ok(()) => result(&bench, "ns", &mut ns, &[("call_bytes", len.to_string()), ("at", off.to_string())]),
        Err(e) => refuse(&bench, e),
    }
}

/// One positioned read of `len` bytes into memory `fresh` makes for it, which
/// nothing has touched: ns per read, the memory's making and release not
/// counted.
fn pread_fresh<M>(bench: &str, f: &File, len: usize, samples: usize, fresh: impl Fn() -> std::io::Result<M>, bytes: impl Fn(&M) -> *mut u8) {
    let pass = |n: usize, keep: &mut Vec<u64>| -> std::io::Result<()> {
        for i in 0..n {
            let memory = fresh()?;
            // SAFETY: `len` bytes of `memory`, which this alone reaches until it goes.
            let into = unsafe { std::slice::from_raw_parts_mut(bytes(&memory), len) };
            let t = Instant::now();
            let got = f.read_at(into, nth(i, len))?;
            keep.push(ns_since(t));
            if got != len {
                return Err(std::io::Error::other(format!("read {got} of {len}")));
            }
        }
        Ok(())
    };
    let mut ns = Vec::new();
    let run = pass(samples / 10 + 1, &mut Vec::new()).and_then(|()| {
        crate::clock::settle();
        pass(samples, &mut ns)
    });
    match run {
        Ok(()) => result(bench, "ns", &mut ns, &[("call_bytes", len.to_string())]),
        Err(e) => refuse(bench, e),
    }
}

fn cached() {
    let target = crate::files::memory();
    let scratch = match crate::files::Scratch::new(&target.dir) {
        Ok(s) => s,
        Err(e) => return refuse("ab.fs.memory", e),
    };
    let big = scratch.at("ab-big");
    let opened = pattern(&big, FILE).and_then(|()| File::open(&big));
    match opened {
        Ok(f) => {
            // Once through, so every page is in memory.
            let mut all = vec![0u8; MIB];
            let mut f2 = &f;
            while f2.read(&mut all).is_ok_and(|n| n > 0) {}
            for (len, samples) in [(8, 2000), (4096, 2000), (65536, 500), (256 * 1024, 200), (MIB, 100)] {
                for off in [0, 16, 64] {
                    if off == 0 || len >= 4096 {
                        pread(&f, len, off, samples);
                    }
                }
            }
            for (len, samples) in [(65536, 200), (MIB, 100), (FILE, 20)] {
                pread_fresh(&format!("ab.fs.memory.fresh.{len}"), &f, len, samples, || Ok(Mapping::huge(len)), Mapping::ptr);
                #[cfg(target_os = "toyos")]
                pread_fresh(
                    &format!("ab.fs.memory.shm.{len}"),
                    &f,
                    len,
                    samples,
                    || toyos::shm::SharedMemory::create(len).map_err(|e| std::io::Error::other(format!("{e:?}"))),
                    toyos::shm::SharedMemory::as_ptr,
                );
            }
        }
        Err(e) => refuse("ab.fs.memory.pread", e),
    }
    let _ = std::fs::remove_file(&big);

    // `persist.seq_read_cached`'s file: 8 MiB written in 1 MiB calls and
    // synced, then read back whole in 1 MiB calls, ten times after one.
    let seq = scratch.at("ab-seq");
    let bench = "ab.fs.memory.fresh8m.seq_read";
    let chunk = vec![0x2du8; MIB];
    let written = (|| -> std::io::Result<()> {
        let mut f = File::create(&seq)?;
        for _ in 0..8 {
            f.write_all(&chunk)?;
        }
        f.sync_all()
    })();
    if let Err(e) = written {
        return refuse(bench, e);
    }
    let mut buf = vec![0u8; MIB];
    let buf_at = buf.as_ptr() as usize % PAGE;
    let mut read_whole = || -> std::io::Result<u64> {
        let t = Instant::now();
        let mut f = File::open(&seq)?;
        let mut got = 0;
        loop {
            match f.read(&mut buf)? {
                0 => break,
                n => got += n,
            }
        }
        if got != 8 * MIB {
            return Err(std::io::Error::other(format!("read {got} bytes")));
        }
        Ok(8 * MIB as u64 * 1000 / ns_since(t).max(1))
    };
    let run = read_whole().and_then(|_| {
        crate::clock::settle();
        (0..10).map(|_| read_whole()).collect::<std::io::Result<Vec<u64>>>()
    });
    match run {
        Ok(mut mbps) => result(bench, "MB/s", &mut mbps, &[("buf_at", buf_at.to_string())]),
        Err(e) => refuse(bench, e),
    }
    let _ = std::fs::remove_file(&seq);
}

/// The library's `memcpy`; `black_box` keeps it a call.
unsafe fn libc(d: *mut u8, s: *const u8, n: usize) {
    // SAFETY: as the caller promises.
    unsafe { std::ptr::copy_nonoverlapping(std::hint::black_box(s), std::hint::black_box(d), std::hint::black_box(n)) }
}

/// #1133's rows as b76abc3bd ran them: `mem.memcpy`'s 256 KiB row, 64 MiB a
/// round and 15 rounds, between two fresh 256 KiB mappings after a touched
/// ballast of 17 x k pages, so each k copies between other physical pages.
fn placements() {
    const SIZE: usize = 256 * 1024;
    for k in 0..8usize {
        let mut ballast = (k > 0).then(|| Mapping::huge(17 * k * 4096));
        if let Some(b) = ballast.as_mut() {
            b.words().fill(7);
        }
        let mut src = Mapping::huge(SIZE);
        let mut dst = Mapping::huge(SIZE);
        for (i, w) in src.words().iter_mut().enumerate() {
            *w = i as u64;
        }
        dst.words().fill(1);
        let (s, d) = (src.ptr(), dst.ptr());
        note(&format!("copyat.place.p{k}.at"), &[("src", format!("{:#x}", s as usize)), ("dst", format!("{:#x}", d as usize))]);
        let copies = 64 * MIB / SIZE;
        let round = || {
            let t = Instant::now();
            for _ in 0..copies {
                // SAFETY: two distinct `SIZE` mappings.
                unsafe { libc(std::hint::black_box(d), std::hint::black_box(s), SIZE) };
            }
            (copies * SIZE) as u64 * 1000 / crate::ns_since(t).max(1)
        };
        round();
        crate::clock::settle();
        let mut mbps: Vec<u64> = (0..15).map(|_| round()).collect();
        result(&format!("copyat.place.p{k}"), "MB/s", &mut mbps, &[("ballast_pages", (17 * k).to_string())]);
    }
}

pub fn suite() {
    for size in [64, 256, 1024, 4096, 16 * 1024, 64 * 1024] {
        rounds(&format!("ab.pipe.natural.{size}"), size, 5, || round(size, Place::Natural, Place::Natural));
    }
    for size in [4096, 16 * 1024, 64 * 1024] {
        for (r, w) in [(0, 0), (16, 0), (32, 0), (64, 0), (2048, 0), (0, 16)] {
            rounds(&format!("ab.pipe.{size}.r{r}.w{w}"), size, 3, || round(size, Place::At(r), Place::At(w)));
        }
    }
    for size in [64, 256, 1024, 4096, 64 * 1024] {
        rounds(&format!("ab.pipe.thread.{size}"), size, 5, || round(size, Place::At(0), Place::At(0)));
        rounds(&format!("ab.pipe.child.{size}"), size, 5, || child_round(size, 0));
    }
    for size in [64, 256, 1024, 4096, 16 * 1024, 64 * 1024] {
        fill_drain(size, 0);
    }
    for size in [4096, 64 * 1024] {
        fill_drain(size, 16);
    }
    cached();
    placements();
    crate::procs::suite();
}
