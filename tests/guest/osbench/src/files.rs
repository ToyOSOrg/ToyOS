//! Files, through std's `fs` on both systems: the same operations under a
//! filesystem each keeps in memory (`mem`), and on ToyOS under `/home`
//! (`served`). Persistent storage is `storage`'s.
//!
//! `mem` is Linux's `/dev/shm` through the VFS, and ToyOS's `/tmp`, which the
//! kernel serves through syscalls. `served` is ToyOS's DATA file server,
//! through IPC per request: on the volume the bench's DATA partition holds
//! where it holds one of ours, and in memory where it does not (the file
//! server says which in the log).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::measure::{each, io, throughput};
use crate::report::{condition, refuse};

const MIB: usize = 1 << 20;

/// A directory to measure under, what serves it, and the medium beneath,
/// where the program can tell: `memory`, or the disk's kind.
pub struct Target {
    pub name: &'static str,
    pub dir: String,
    pub served_by: String,
    pub medium: Option<String>,
}

impl Target {
    /// What every line measured under the target carries.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![("served_by", self.served_by.clone())];
        fields.extend(self.medium.clone().map(|m| ("medium", m)));
        fields
    }
}

pub fn memory() -> Target {
    let (dir, served_by) = if cfg!(target_os = "toyos") {
        ("/tmp", "the kernel's tmpfs, through syscalls")
    } else {
        ("/dev/shm", "tmpfs, through the VFS")
    };
    Target { name: "mem", dir: dir.into(), served_by: served_by.into(), medium: Some("memory".into()) }
}

/// Every directory `files` measures under.
pub fn targets() -> Vec<Target> {
    let mut targets = vec![memory()];
    if cfg!(target_os = "toyos") {
        // Memory or the DATA partition's volume: the program cannot tell which.
        let served_by = "the DATA file server, through IPC".into();
        targets.push(Target { name: "served", dir: "/home".into(), served_by, medium: None });
    }
    targets
}

/// The persistent filesystem's directory, or `None` where the system has none.
pub fn disk() -> Option<Target> {
    crate::sys::persistent_dir().map(|(dir, served_by, medium)| Target { name: "disk", dir, served_by, medium: Some(medium) })
}

/// A scratch directory of this process's under `dir`, removed whole on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(dir: &str) -> std::io::Result<Scratch> {
        let path = Path::new(dir).join(format!("osbench-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path)?;
        Ok(Scratch(path))
    }

    pub fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A file of `len` bytes of a pattern.
fn fill(path: &Path, len: usize) -> std::io::Result<()> {
    let block: Vec<u8> = (0..MIB.min(len)).map(|i| (i % 251) as u8).collect();
    let mut f = File::create(path)?;
    let mut left = len;
    while left > 0 {
        let n = left.min(block.len());
        f.write_all(&block[..n])?;
        left -= n;
    }
    Ok(())
}

/// A 4 KiB-aligned offset in a file of `pages` pages, the same sequence on
/// both systems.
fn page_at(i: usize, pages: usize) -> u64 {
    let x = (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 20;
    (x % pages as u64) * 4096
}

/// Every benchmark under `target`, each line naming what served it and its
/// medium. Each one's comment is its warm-up and repetitions.
pub fn suite(target: &Target) {
    condition(&format!("fs.{}.served_by", target.name), &target.served_by);
    #[cfg(target_os = "linux")]
    condition(&format!("fs.{}.mount", target.name), crate::sys::mount_options(&target.dir));
    let scratch = match Scratch::new(&target.dir) {
        Ok(s) => s,
        Err(e) => return refuse(&format!("fs.{}", target.name), format!("no scratch directory under {}: {e}", target.dir)),
    };
    let name = |op: &str| format!("fs.{}.{op}", target.name);
    let with = |fields: &[(&'static str, String)]| -> Vec<(&'static str, String)> {
        fields.iter().cloned().chain(target.fields()).collect()
    };

    // Opening an existing 4 KiB file for reading and closing it: 2,000 after 100.
    let small = scratch.at("small");
    if let Err(e) = fill(&small, 4096) {
        return refuse(&name("setup"), e);
    }
    each(&name("open_close"), 100, 2000, &with(&[]), |_| io("open", File::open(&small)).map(drop));

    // `metadata` of the same file: 2,000 after 100.
    each(&name("stat"), 100, 2000, &with(&[]), |_| io("stat", fs::metadata(&small)).map(drop));

    // A new file, 100 bytes written to it, closed and removed: 1,000 after 50.
    let fresh = scratch.at("fresh");
    each(&name("create_write_unlink"), 50, 1000, &with(&[("bytes", "100".into())]), |_| {
        let mut f = io("create", File::create(&fresh))?;
        io("write", f.write_all(&[7u8; 100]))?;
        drop(f);
        io("unlink", fs::remove_file(&fresh))
    });

    // A 4 KiB read and a 4 KiB write at a page-aligned offset of a 16 MiB
    // file held open, each a seek and the transfer (std on ToyOS has no
    // positioned read): 2,000 after 100.
    let big = scratch.at("big");
    if let Err(e) = fill(&big, 16 * MIB) {
        return refuse(&name("setup"), e);
    }
    let pages = 16 * MIB / 4096;
    match OpenOptions::new().read(true).write(true).open(&big) {
        Ok(mut f) => {
            let mut buf = vec![0u8; 4096];
            each(&name("read_4k_at"), 100, 2000, &with(&[("file_bytes", (16 * MIB).to_string())]), |i| {
                io("seek", f.seek(SeekFrom::Start(page_at(i, pages))))?;
                io("read", f.read_exact(&mut buf))
            });
            each(&name("write_4k_at"), 100, 2000, &with(&[("file_bytes", (16 * MIB).to_string())]), |i| {
                io("seek", f.seek(SeekFrom::Start(page_at(i, pages))))?;
                io("write", f.write_all(&buf))
            });
        }
        Err(e) => refuse(&name("read_4k_at"), e),
    }
    let _ = fs::remove_file(&big);

    // Listing a directory of 1,000 files, every name: 50 after 5.
    let listed = scratch.at("listed");
    let made = (|| -> std::io::Result<()> {
        fs::create_dir(&listed)?;
        for i in 0..1000 {
            File::create(listed.join(format!("entry-{i:04}")))?;
        }
        Ok(())
    })();
    match made {
        Ok(()) => each(&name("readdir.1000"), 5, 50, &with(&[("entries", "1000".into())]), |_| {
            let mut n = 0;
            for entry in io("read_dir", fs::read_dir(&listed))? {
                std::hint::black_box(io("entry", entry)?.file_name());
                n += 1;
            }
            if n == 1000 { Ok(()) } else { Err(format!("listed {n} of 1000")) }
        }),
        Err(e) => refuse(&name("readdir.1000"), e),
    }
    let _ = fs::remove_dir_all(&listed);

    // A file renamed within its directory, back and forth: 2,000 after 100.
    let (a, b) = (scratch.at("a"), scratch.at("b"));
    match File::create(&a) {
        Ok(_) => each(&name("rename"), 100, 2000, &with(&[]), |i| {
            let (from, to) = if i % 2 == 0 { (&a, &b) } else { (&b, &a) };
            io("rename", fs::rename(from, to))
        }),
        Err(e) => refuse(&name("rename"), e),
    }

    // An empty directory made and removed: 1,000 after 50.
    let sub = scratch.at("sub");
    each(&name("mkdir_rmdir"), 50, 1000, &with(&[]), |_| {
        io("mkdir", fs::create_dir(&sub))?;
        io("rmdir", fs::remove_dir(&sub))
    });

    // A directory of 100 files of 64 bytes made, then every file and the
    // directory removed: ns per whole tree, 20 after 2.
    let tree = scratch.at("tree");
    each(&name("tree.100"), 2, 20, &with(&[("files", "100".into())]), |_| {
        io("mkdir", fs::create_dir(&tree))?;
        for i in 0..100 {
            io("write", fs::write(tree.join(format!("f{i:03}")), [3u8; 64]))?;
        }
        for i in 0..100 {
            io("unlink", fs::remove_file(tree.join(format!("f{i:03}"))))?;
        }
        io("rmdir", fs::remove_dir(&tree))
    });

    // 64 MiB written to a new file and read back whole, in 4 KiB and 1 MiB
    // calls: MB/s, five rounds after one. Last, as the largest: a server that
    // cannot hold it takes no other benchmark's number with it.
    sequential(&name("seq"), &scratch.at("seq"), 64 * MIB, 5, &with(&[]));
}

/// `len` bytes written to `path` and read back, in 4 KiB and in 1 MiB calls:
/// `<prefix>_write.<call>` and `<prefix>_read.<call>`, MB/s, `rounds` after
/// one, each line with `fields` beside its own. A write round creates the
/// file anew; the read rounds read the last one written.
fn sequential(prefix: &str, path: &Path, len: usize, rounds: usize, fields: &[(&'static str, String)]) {
    let block: Vec<u8> = (0..MIB).map(|i| (i % 253) as u8).collect();
    for call in [4096usize, MIB] {
        let extra: Vec<_> =
            [("call_bytes", call.to_string()), ("file_bytes", len.to_string())].into_iter().chain(fields.iter().cloned()).collect();
        throughput(&format!("{prefix}_write.{call}"), rounds, len as u64, &extra, || {
            let mut f = io("create", File::create(path))?;
            for chunk in 0..len / call {
                let at = chunk * call % MIB;
                io("write", f.write_all(&block[at..at + call]))?;
            }
            Ok(())
        });
        let mut buf = vec![0u8; call];
        throughput(&format!("{prefix}_read.{call}"), rounds, len as u64, &extra, || {
            let mut f = io("open", File::open(path))?;
            let mut got = 0;
            loop {
                match io("read", f.read(&mut buf))? {
                    0 => break,
                    n => got += n,
                }
            }
            if got == len { Ok(()) } else { Err(format!("read {got} of {len} bytes")) }
        });
    }
    let _ = fs::remove_file(path);
}
