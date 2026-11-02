//! The primitives every program pays for: a syscall, a thread, a process, a
//! pipe, anonymous memory and the memory bus.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::report::{refuse, result};
use crate::sys::{self, Mapping};
use crate::{chunk, ns_since};

const MIB: usize = 1 << 20;

/// What one chunk of the fairness work costs one thread, 200 after a settle:
/// the same instructions on both systems, so a difference between them is the
/// clock that thread ran at.
pub fn chunk_rate() {
    chunk();
    crate::clock::settle();
    let mut ns: Vec<u64> = (0..200)
        .map(|_| {
            let t = Instant::now();
            chunk();
            ns_since(t)
        })
        .collect();
    result("cpu.chunk", "ns", &mut ns, &[]);
}

/// The cheapest syscall each kernel has, getpid on both: 15 rounds of
/// 100,000 calls after one unread round, picoseconds per call.
pub fn null_syscall() {
    const CALLS: u64 = 100_000;
    let round = || {
        let t = Instant::now();
        for _ in 0..CALLS {
            sys::null_syscall();
        }
        ns_since(t) * 1000 / CALLS
    };
    round();
    crate::clock::settle();
    let mut picos: Vec<u64> = (0..15).map(|_| round()).collect();
    result("syscall.null", "ps", &mut picos, &[("calls_per_sample", CALLS.to_string())]);
}

/// `thread::spawn` of an empty closure and its `join`: 1,000 after 50.
pub fn thread_spawn() {
    let span = crate::ran::start();
    let once = || {
        let t = Instant::now();
        thread::spawn(|| {}).join().expect("an empty thread");
        ns_since(t)
    };
    for _ in 0..50 {
        once();
    }
    crate::clock::settle();
    let mut ns: Vec<u64> = (0..1000).map(|_| once()).collect();
    result("thread.spawn_join", "ns", &mut ns, &[("parent", crate::ran::thread_over(span, sys::tid()))]);
}

/// This program spawned with null stdio as `osbench nop`, which exits at
/// once, and waited for: 100 after 10.
pub fn process_spawn() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return refuse("process.spawn_wait", format!("no path to this program: {e}")),
    };
    let once = || -> Result<u64, String> {
        let t = Instant::now();
        let status = Command::new(&exe)
            .arg("nop")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("spawn {}: {e}", exe.display()))?;
        let ns = ns_since(t);
        if !status.success() {
            return Err(format!("nop ended {status}"));
        }
        Ok(ns)
    };
    let run = || -> Result<Vec<u64>, String> {
        for _ in 0..10 {
            once()?;
        }
        crate::clock::settle();
        (0..100).map(|_| once()).collect()
    };
    match run() {
        Ok(mut ns) => result("process.spawn_wait", "ns", &mut ns, &[]),
        Err(why) => refuse("process.spawn_wait", why),
    }
}

/// One thread writing `size`-byte writes into a pipe for 100 ms and another
/// reading it in 64 KiB reads: MB/s as the reader saw it from its first byte
/// to end-of-file. One unread round, then seven.
pub fn pipe_throughput(size: usize) {
    let bench = format!("pipe.throughput.{size}");
    let rounds = std::cell::RefCell::new(Vec::new());
    let round = || -> std::io::Result<u64> {
        let span = crate::ran::start();
        let (mut reader, mut writer) = std::io::pipe()?;
        let writing = thread::spawn(move || -> std::io::Result<sys::Tid> {
            let chunk = vec![0x5au8; size];
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(100) {
                for _ in 0..64 {
                    writer.write_all(&chunk)?;
                }
            }
            Ok(sys::tid())
        });
        let mut buf = vec![0u8; 64 * 1024];
        // The clock starts at the first byte; what it carried is not counted.
        if reader.read(&mut buf)? == 0 {
            return Err(std::io::Error::other("the writer ended before writing"));
        }
        let (mut bytes, start) = (0u64, Instant::now());
        loop {
            match reader.read(&mut buf)? {
                0 => break,
                n => bytes += n as u64,
            }
        }
        let ns = ns_since(start).max(1);
        let writer_tid = writing.join().expect("the writer panicked")?;
        let mbps = bytes * 1000 / ns;
        rounds.borrow_mut().push(crate::ran::pipe_round(span, mbps, sys::tid(), writer_tid));
        Ok(mbps)
    };
    let run = || -> std::io::Result<Vec<u64>> {
        round()?;
        crate::clock::settle();
        (0..7).map(|_| round()).collect()
    };
    match run() {
        Ok(mut mbps) => result(&bench, "MB/s", &mut mbps, &[("write_bytes", size.to_string()), ("rounds", rounds.borrow().join(","))]),
        Err(e) => refuse(&bench, e),
    }
}

/// Anonymous memory made usable and given back, ns per MiB of a 32 MiB
/// mapping: the `mmap`, a write to every 4 KiB page, and the `munmap`, each
/// timed alone and summed. Both systems fault each page in on its first touch. One unread round, then ten.
pub fn anon_memory() {
    const LEN: usize = 32 * MIB;
    const MIBS: u64 = (LEN / MIB) as u64;
    let round = || {
        let t = Instant::now();
        let ptr = sys::map_anon(LEN);
        let map = ns_since(t);
        let t = Instant::now();
        for page in 0..LEN / 4096 {
            // SAFETY: inside the mapping just made.
            unsafe { ptr.add(page * 4096).write_volatile(1) };
        }
        let touch = ns_since(t);
        let t = Instant::now();
        // SAFETY: the whole mapping, unused from here.
        unsafe { sys::unmap(ptr, LEN) };
        let unmap = ns_since(t);
        [map, touch, unmap]
    };
    round();
    crate::clock::settle();
    let rounds: Vec<[u64; 3]> = (0..10).map(|_| round()).collect();
    let per_mib = |pick: &dyn Fn(&[u64; 3]) -> u64| -> Vec<u64> { rounds.iter().map(|r| pick(r) / MIBS).collect() };
    let extra = [("mapping_bytes", LEN.to_string())];
    result("mem.anon.map", "ns/MiB", &mut per_mib(&|r| r[0]), &extra);
    result("mem.anon.touch", "ns/MiB", &mut per_mib(&|r| r[1]), &extra);
    result("mem.anon.unmap", "ns/MiB", &mut per_mib(&|r| r[2]), &extra);
    result("mem.anon.total", "ns/MiB", &mut per_mib(&|r| r.iter().sum()), &extra);
}

/// A 4 KiB anonymous `mmap` and its `munmap`, never touched: 2,000 after 50.
pub fn map_unmap_small() {
    let once = || {
        let t = Instant::now();
        let ptr = sys::map_anon(4096);
        // SAFETY: the whole mapping, never used.
        unsafe { sys::unmap(ptr, 4096) };
        ns_since(t)
    };
    for _ in 0..50 {
        once();
    }
    crate::clock::settle();
    let mut ns: Vec<u64> = (0..2000).map(|_| once()).collect();
    result("mem.map_unmap_4k", "ns", &mut ns, &[]);
}

/// One thread's bandwidth over two 64 MiB buffers, far past either bench's
/// last-level cache, in MB/s of buffer covered: `read` sums it, `write`
/// stores a pattern, and `copy` stores the source xor a constant — a loop the
/// compiler cannot turn into a `memcpy` call, so both systems run the same
/// instructions. Each system's own `memcpy` is `mem.memcpy.*`'s.
/// Both are asked for huge pages; the line says how much each gave, and
/// ToyOS gives none yet. One unread round, then seven, per kind.
pub fn bandwidth() {
    const LEN: usize = 64 * MIB;
    let mut src = Mapping::huge(LEN);
    let mut dst = Mapping::huge(LEN);
    for (i, w) in src.words().iter_mut().enumerate() {
        *w = i as u64;
    }
    dst.words().fill(1);
    let huge = [("huge_pages", sys::huge_backing())];

    let kind = |name: &str, f: &mut dyn FnMut()| {
        let mut round = || {
            let t = Instant::now();
            f();
            (LEN as u64) * 1000 / ns_since(t).max(1)
        };
        round();
        crate::clock::settle();
        let mut mbps: Vec<u64> = (0..7).map(|_| round()).collect();
        result(&format!("mem.bw.{name}"), "MB/s", &mut mbps, &huge);
    };
    kind("read", &mut || {
        let sum = src.words().iter().fold(0u64, |a, &w| a.wrapping_add(w));
        std::hint::black_box(sum);
    });
    let mut seed = 0u64;
    kind("write", &mut || {
        seed += 1;
        for (i, w) in dst.words().iter_mut().enumerate() {
            *w = seed ^ i as u64;
        }
        std::hint::black_box(dst.ptr());
    });
    kind("copy", &mut || {
        let (s, d) = (src.words(), dst.words());
        for (d, s) in d.iter_mut().zip(s.iter()) {
            *d = *s ^ 0x9e37_79b9_7f4a_7c15;
        }
        std::hint::black_box(dst.ptr());
    });
}
