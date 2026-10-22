//! Persistent storage, each system on the medium it has, so **not like for
//! like**: what these say is what a durable write costs on the storage a
//! measured boot has, never which kernel is faster.
//!
//! On a ToyOS benchcase boot that is the boot USB stick's `TOYOS-LOG`
//! partition, FAT32, through the LOG file server and usbd: the boot has no
//! DATA partition, so `/home` and `/state` are memory (`files`'s `served` is `HOME`),
//! and the stick's other volumes are read-only. On Linux it is a directory in
//! the runner's home, on whatever disk and filesystem hold it, which the
//! conditions name. Every file here is removed at the end; the most alive at
//! once is under 10 MiB, inside what the log volume leaves free.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

use crate::files::Scratch;
use crate::measure::{each, io, throughput};
use crate::report::{condition, refuse};
use crate::sys;

const MIB: usize = 1 << 20;

pub fn suite() {
    let Some((dir, medium)) = sys::persistent_dir() else {
        return refuse("persist", "no persistent directory to write in");
    };
    condition("persist.medium", medium);
    let scratch = match Scratch::new(&dir) {
        Ok(s) => s,
        Err(e) => return refuse("persist", format!("no scratch directory: {e}")),
    };
    let block = [0x6bu8; 4096];

    // 4 KiB written over the start of a 1 MiB file, then `sync_all`: 100 after 5.
    let file = scratch.at("over");
    let made = (|| -> std::io::Result<File> {
        fs::write(&file, vec![1u8; MIB])?;
        OpenOptions::new().read(true).write(true).open(&file)
    })();
    match made {
        Ok(mut f) => {
            each("persist.write_4k_fsync", 5, 100, &[], |_| {
                io("seek", f.seek(SeekFrom::Start(0)))?;
                io("write", f.write_all(&block))?;
                io("fsync", f.sync_all())
            });
            // `sync_all` of the same file with nothing new in it: 200 after 10.
            each("persist.fsync_clean", 10, 200, &[], |_| io("fsync", f.sync_all()));
        }
        Err(e) => refuse("persist.write_4k_fsync", e),
    }

    // 4 KiB appended, then `sync_data`: 100 after 5, so the file grows by 420 KiB.
    match OpenOptions::new().create(true).append(true).open(scratch.at("log")) {
        Ok(mut f) => each("persist.append_4k_fsync", 5, 100, &[], |_| {
            io("write", f.write_all(&block))?;
            io("fsync", f.sync_data())
        }),
        Err(e) => refuse("persist.append_4k_fsync", e),
    }

    // A new file, 4 KiB written and `sync_all`ed, closed and removed: 50
    // after 5. The directory is not synced.
    let fresh = scratch.at("fresh");
    each("persist.create_fsync_unlink", 5, 50, &[], |_| {
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
    let extra = [("file_bytes", (8 * MIB).to_string()), ("call_bytes", MIB.to_string())];
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
