//! SCRATCH, never lands: where a pipe's and a cached read's time goes.
//!
//! The same rows on both systems; on ToyOS each is followed by the kernel's
//! stage timers over one more pass (`kernel/src/probe.rs`).

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::File;
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::ns_since;
use crate::report::{note, refuse, result};

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

#[cfg(all(target_os = "toyos", target_arch = "x86_64"))]
mod stage {
    const SYSCALL: u64 = 0xffff_9199;
    pub const STAGES: usize = 48;

    fn call(a1: u64, a2: u64, a3: u64) -> u64 {
        let ret: u64;
        // SAFETY: the scratch kernel's probe call; `a2` names a live buffer of `a3` bytes for op 2.
        unsafe {
            std::arch::asm!(
                "syscall",
                in("rdi") SYSCALL,
                in("rsi") a1,
                in("rdx") a2,
                in("r8") a3,
                in("r9") 0u64,
                lateout("rax") ret,
                out("rcx") _,
                out("r11") _,
            );
        }
        ret
    }

    pub fn on() {
        call(1, 0, 0);
    }

    pub fn off() -> Vec<(u64, u64)> {
        call(0, 0, 0);
        let mut words = [0u64; 2 * STAGES];
        call(2, words.as_mut_ptr() as u64, (16 * STAGES) as u64);
        words.chunks(2).map(|w| (w[0], w[1])).collect()
    }

    pub fn tsc() -> u64 {
        // SAFETY: reads the counter.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
}

/// The kernel's stage timers over `pass`, as one note: `s<id>=<cycles>/<count>`.
fn staged(row: &str, pass: impl FnOnce()) {
    #[cfg(all(target_os = "toyos", target_arch = "x86_64"))]
    {
        let (t, c) = (Instant::now(), stage::tsc());
        stage::on();
        pass();
        let stages = stage::off();
        let (ns, cycles) = (ns_since(t), stage::tsc() - c);
        let mut fields: Vec<(String, String)> = vec![("row".into(), row.into()), ("pass_ns".into(), ns.to_string()), ("pass_tsc".into(), cycles.to_string())];
        for (id, (cycles, count)) in stages.iter().enumerate() {
            if *count > 0 {
                fields.push((format!("s{id}"), format!("{cycles}/{count}")));
            }
        }
        let fields: Vec<(&str, String)> = fields.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        note("stages", &fields);
    }
    #[cfg(not(all(target_os = "toyos", target_arch = "x86_64")))]
    {
        let _ = row;
        pass();
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

/// `system::pipe_throughput`'s round, counting its calls.
fn round(size: usize, reader: Place, writer: Place) -> std::io::Result<Round> {
    let (mut rx, mut tx) = std::io::pipe()?;
    let writing = thread::spawn(move || -> std::io::Result<(u64, usize)> {
        let mut chunk = Buf::new(writer, size, 0x5a);
        let chunk = chunk.bytes();
        let start = Instant::now();
        let mut writes = 0u64;
        while start.elapsed() < Duration::from_millis(100) {
            for _ in 0..64 {
                tx.write_all(chunk)?;
            }
            writes += 64;
        }
        Ok((writes, chunk.as_ptr() as usize % PAGE))
    });
    let mut buf = Buf::new(reader, 64 * 1024, 0);
    let buf = buf.bytes();
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
    let ns = ns_since(start).max(1);
    let (writes, writer_at) = writing.join().expect("the writer panicked")?;
    Ok(Round { mbps: bytes * 1000 / ns, reads, bytes, writes, reader_at: buf.as_ptr() as usize % PAGE, writer_at })
}

fn throughput(bench: &str, size: usize, reader: Place, writer: Place, rounds: usize) {
    let run = || -> std::io::Result<Vec<Round>> {
        round(size, reader, writer)?;
        crate::clock::settle();
        let before = crate::sys::counts();
        let rounds: Vec<Round> = (0..rounds).map(|_| round(size, reader, writer)).collect::<Result<_, _>>()?;
        let after = crate::sys::counts();
        let writes: u64 = rounds.iter().map(|r| r.writes).sum();
        let reads: u64 = rounds.iter().map(|r| r.reads).sum();
        let bytes: u64 = rounds.iter().map(|r| r.bytes).sum();
        let mut fields = vec![
            ("write_bytes", size.to_string()),
            ("reads", reads.to_string()),
            ("writes", writes.to_string()),
            ("bytes_per_read", (bytes / reads.max(1)).to_string()),
            ("reader_at", rounds[0].reader_at.to_string()),
            ("writer_at", rounds[0].writer_at.to_string()),
        ];
        if let (Some(a), Some(b)) = (before.switches, after.switches) {
            fields.push(("switches", (b - a).to_string()));
        }
        let mut mbps: Vec<u64> = rounds.iter().map(|r| r.mbps).collect();
        result(bench, "MB/s", &mut mbps, &fields);
        Ok(rounds)
    };
    if let Err(e) = run() {
        return refuse(bench, e);
    }
    staged(bench, || match round(size, reader, writer) {
        Ok(r) => note(
            "staged_round",
            &[
                ("row", bench.to_string()),
                ("mbps", r.mbps.to_string()),
                ("reads", r.reads.to_string()),
                ("writes", r.writes.to_string()),
                ("bytes", r.bytes.to_string()),
            ],
        ),
        Err(e) => refuse(bench, e),
    });
}

/// One thread fills an empty pipe with 64 KiB in `size`-byte writes and
/// empties it in `size`-byte reads: ns per write and per read, no wait and no
/// wake in either.
fn fill_drain(size: usize, off: usize) {
    const TOTAL: usize = 64 * 1024;
    let calls = TOTAL / size;
    let suffix = if off == 0 { String::new() } else { format!(".d{off}") };
    let (wname, rname) = (format!("probe.pipe.fill.{size}{suffix}"), format!("probe.pipe.drain.{size}{suffix}"));
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
        Err(e) => return refuse(&wname, e),
    }
    staged(&wname, || {
        let _ = run(100);
    });
}

fn pattern(path: &std::path::Path, len: usize) -> std::io::Result<()> {
    let block: Vec<u8> = (0..MIB).map(|i| (i % 251) as u8).collect();
    let mut f = File::create(path)?;
    for _ in 0..len / MIB {
        f.write_all(&block)?;
    }
    Ok(())
}

/// Positioned reads of `len` bytes into a buffer `off` bytes into a page,
/// at page-aligned offsets in order through a 16 MiB file: ns per read.
fn pread(name: &str, f: &File, len: usize, off: usize, samples: usize) {
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::FileExt;
    #[cfg(target_os = "toyos")]
    use std::os::toyos::fs::FileExt;

    const FILE: usize = 16 * MIB;
    let bench = format!("probe.fs.{name}.pread.{len}.d{off}");
    let mut buf = Aligned::new(len, off, 0);
    let step = len.max(PAGE);
    let mut pass = |n: usize, keep: &mut Vec<u64>| -> std::io::Result<()> {
        for i in 0..n {
            let at = (i * step % (FILE - step + PAGE)) as u64 / PAGE as u64 * PAGE as u64;
            let t = Instant::now();
            let got = f.read_at(buf.bytes(), at)?;
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
        Err(e) => return refuse(&bench, e),
    }
    if off == 0 {
        staged(&bench, || {
            let _ = pass(samples, &mut Vec::new());
        });
    }
}

fn cached(target: &crate::files::Target) {
    let scratch = match crate::files::Scratch::new(&target.dir) {
        Ok(s) => s,
        Err(e) => return refuse(&format!("probe.fs.{}", target.name), e),
    };
    let big = scratch.at("probe-big");
    let opened = pattern(&big, 16 * MIB).and_then(|()| File::open(&big));
    match opened {
        Ok(f) => {
            // Once through, so every page is in memory on both systems.
            let mut all = vec![0u8; MIB];
            let mut f2 = &f;
            while f2.read(&mut all).is_ok_and(|n| n > 0) {}
            for (len, samples) in [(8, 2000), (4096, 2000), (65536, 500), (MIB, 100)] {
                for off in [0, 16, 64] {
                    if off == 0 || len >= 4096 {
                        pread(target.name, &f, len, off, samples);
                    }
                }
            }
        }
        Err(e) => refuse(&format!("probe.fs.{}.pread", target.name), e),
    }
    let _ = std::fs::remove_file(&big);

    // `persist.seq_read_cached`'s file: 8 MiB written in 1 MiB calls and
    // synced, then read back whole in 1 MiB calls, ten times after one.
    let seq = scratch.at("probe-seq");
    let bench = format!("probe.fs.{}.fresh8m.seq_read", target.name);
    let chunk = vec![0x2du8; MIB];
    let written = (|| -> std::io::Result<()> {
        let mut f = File::create(&seq)?;
        for _ in 0..8 {
            f.write_all(&chunk)?;
        }
        f.sync_all()
    })();
    if let Err(e) = written {
        return refuse(&bench, e);
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
        Ok(mut mbps) => result(&bench, "MB/s", &mut mbps, &[("buf_at", buf_at.to_string())]),
        Err(e) => return refuse(&bench, e),
    }
    staged(&bench, || {
        for _ in 0..5 {
            let _ = read_whole();
        }
    });
    let _ = std::fs::remove_file(&seq);
}

pub fn suite() {
    for size in [64, 256, 1024, 4096, 16 * 1024, 64 * 1024] {
        throughput(&format!("probe.pipe.natural.{size}"), size, Place::Natural, Place::Natural, 5);
    }
    for size in [4096, 16 * 1024, 64 * 1024] {
        for (r, w) in [(0, 0), (16, 0), (32, 0), (64, 0), (2048, 0), (0, 16)] {
            throughput(&format!("probe.pipe.{size}.r{r}.w{w}"), size, Place::At(r), Place::At(w), 3);
        }
    }
    for size in [64, 256, 1024, 4096, 16 * 1024, 64 * 1024] {
        fill_drain(size, 0);
    }
    for size in [4096, 64 * 1024] {
        fill_drain(size, 16);
    }
    cached(&crate::files::memory());
    if let Some(disk) = crate::files::disk() {
        cached(&disk);
    }
}
