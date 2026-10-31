//! The bench's disk, through each system's persistent filesystem
//! ([`crate::files::disk`]): durable writes first, each made durable by
//! `sync_all` or `sync_data` before the next, then `files`' cached operations
//! under the same directory as `fs.disk.*`. Every line names what served it
//! and its medium.
//!
//! On Linux that is the runner's home, on whatever filesystem and disk hold
//! it. On ToyOS it is `/home`, on the DATA volume: the bcachefs crate's
//! format, through the DATA file server, the block service and the disk's
//! driver. A ToyOS boot whose DATA is in memory measures memory here, which
//! the `osbench` metal row refuses (`tests/toyos.rs`). Every file here is
//! removed at the end.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

use crate::files::{self, Scratch};
use crate::measure::{each, io, throughput};
use crate::report::{condition, refuse, result};

const MIB: usize = 1 << 20;

/// How long the disk is left idle before each [`after_idle`] sample: past the
/// idle time either system's drive waits before its deepest power state.
const IDLE: Duration = Duration::from_secs(10);
/// The samples [`after_idle`] takes, each after [`IDLE`].
const AFTER_IDLE_SAMPLES: usize = 8;

/// `op` timed alone, [`AFTER_IDLE_SAMPLES`] times, each after [`IDLE`] in which
/// this program touches nothing: ns per operation.
fn after_idle(bench: &str, served: &[(&str, String)], mut op: impl FnMut() -> Result<(), String>) {
    let mut ns = Vec::with_capacity(AFTER_IDLE_SAMPLES);
    for _ in 0..AFTER_IDLE_SAMPLES {
        std::thread::sleep(IDLE);
        let t = Instant::now();
        if let Err(why) = op() {
            return refuse(bench, why);
        }
        ns.push(u64::try_from(t.elapsed().as_nanos()).expect("an operation under 584 years"));
    }
    let fields: Vec<(&str, String)> = std::iter::once(("idle_s", IDLE.as_secs().to_string())).chain(served.iter().cloned()).collect();
    result(bench, "ns", &mut ns, &fields);
}

pub fn suite() {
    let Some(disk) = files::disk() else {
        return refuse("persist", "no persistent directory to write in");
    };
    durable(&disk);
    files::suite(&disk);
}

fn durable(disk: &files::Target) {
    let scratch = match Scratch::new(&disk.dir) {
        Ok(s) => s,
        Err(e) => return refuse("persist", format!("no scratch directory: {e}")),
    };
    condition("persist.served_by", &disk.served_by);
    let served = disk.fields();
    let block = [0x6bu8; 4096];

    // 4 KiB written over the start of a 1 MiB file, then `sync_all`: 100 after 5.
    let file = scratch.at("over");
    let made = (|| -> std::io::Result<File> {
        fs::write(&file, vec![1u8; MIB])?;
        OpenOptions::new().read(true).write(true).open(&file)
    })();
    match made {
        Ok(mut f) => {
            each("persist.write_4k_fsync", 5, 100, &served, |_| {
                io("seek", f.seek(SeekFrom::Start(0)))?;
                io("write", f.write_all(&block))?;
                io("fsync", f.sync_all())
            });
            // `sync_all` of the same file with nothing new in it: 200 after 10.
            each("persist.fsync_clean", 10, 200, &served, |_| io("fsync", f.sync_all()));
            // The same write and `sync_all`, each after the disk has been
            // left idle: what a drive's idle power states cost the first I/O
            // that wakes it, against `persist.write_4k_fsync`'s back to back.
            after_idle("persist.write_4k_fsync_after_idle", &served, || {
                io("seek", f.seek(SeekFrom::Start(0)))?;
                io("write", f.write_all(&block))?;
                io("fsync", f.sync_all())
            });
        }
        Err(e) => refuse("persist.write_4k_fsync", e),
    }

    // 4 KiB appended, then `sync_data`: 100 after 5, so the file grows by 420 KiB.
    match OpenOptions::new().create(true).append(true).open(scratch.at("log")) {
        Ok(mut f) => each("persist.append_4k_fsync", 5, 100, &served, |_| {
            io("write", f.write_all(&block))?;
            io("fsync", f.sync_data())
        }),
        Err(e) => refuse("persist.append_4k_fsync", e),
    }

    // A new file, 4 KiB written and `sync_all`ed, closed and removed: 50
    // after 5. The directory is not synced.
    let fresh = scratch.at("fresh");
    each("persist.create_fsync_unlink", 5, 50, &served, |_| {
        let mut f = io("create", File::create(&fresh))?;
        io("write", f.write_all(&block))?;
        io("fsync", f.sync_all())?;
        drop(f);
        io("unlink", fs::remove_file(&fresh))
    });

    // 8 MiB written in 1 MiB calls to a new file and `sync_all`ed: MB/s
    // including the sync, three rounds after one; then read back in 1 MiB
    // calls, which both systems answer from their caches.
    let seq = scratch.at("seq");
    let chunk = vec![0x2du8; MIB];
    let extra: Vec<_> =
        [("file_bytes", (8 * MIB).to_string()), ("call_bytes", MIB.to_string())].into_iter().chain(served.iter().cloned()).collect();
    throughput("persist.seq_write_fsync", 3, 8 * MIB as u64, &extra, || {
        let mut f = io("create", File::create(&seq))?;
        for _ in 0..8 {
            io("write", f.write_all(&chunk))?;
        }
        io("fsync", f.sync_all())
    });
    let mut buf = vec![0u8; MIB];
    throughput("persist.seq_read_cached", 3, 8 * MIB as u64, &extra, || {
        let mut f = io("open", File::open(&seq))?;
        let mut got = 0;
        loop {
            match io("read", f.read(&mut buf))? {
                0 => break,
                n => got += n,
            }
        }
        if got == 8 * MIB { Ok(()) } else { Err(format!("read {got} bytes")) }
    });
    let _ = fs::remove_file(&seq);
}
