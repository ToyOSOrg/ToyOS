//! The NVMe namespace's own reads, **read-only**: the device is opened for
//! reading and nothing here writes, so a bench's installed system is never
//! touched. Linux reads its first NVMe namespace's block device with
//! `O_DIRECT`, past its page cache, which takes root; ToyOS grants a program
//! no disk's blocks outside a file server's role and the idle slot.

const BENCHES: [&str; 2] = ["nvme.read.4k.random", "nvme.read.1m.seq"];

pub fn suite() {
    imp::suite();
}

#[cfg(target_os = "toyos")]
mod imp {
    pub fn suite() {
        for bench in super::BENCHES {
            crate::report::unsupported(
                bench,
                "the block service reaches a partition only through a grant the supervisor mints for a file \
                 server's role or the idle slot, and this bench's NVMe holds another system",
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::{self, File, OpenOptions};
    use std::os::unix::fs::{FileExt, OpenOptionsExt};

    use crate::measure::{each, io, throughput};
    use crate::report::{condition, refuse, unmeasured};
    use crate::sys::Mapping;

    const MIB: usize = 1 << 20;
    const PAGE: u64 = 4096;
    /// What one sequential round reads, from a region of its own.
    const SEQ_BYTES: u64 = 256 << 20;

    pub fn suite() {
        let mut disks: Vec<String> = fs::read_dir("/sys/block")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("nvme"))
            .collect();
        disks.sort();
        let Some(disk) = disks.first() else {
            return super::BENCHES.iter().for_each(|b| unmeasured(b, "this machine has no NVMe namespace", &[]));
        };
        let sys = |f: &str| fs::read_to_string(format!("/sys/block/{disk}/{f}")).map(|s| s.trim().to_string()).unwrap_or_default();
        let bytes = sys("size").parse::<u64>().unwrap_or(0) * 512;
        condition("nvme.namespace", disk);
        condition("nvme.model", sys("device/model"));
        condition("nvme.logical_block", sys("queue/logical_block_size"));
        if bytes < 4 * SEQ_BYTES {
            return refuse("nvme", format!("the namespace is {bytes} bytes, under the {} the sequential rounds read", 4 * SEQ_BYTES));
        }
        let path = format!("/dev/{disk}");
        let file = match OpenOptions::new().read(true).custom_flags(libc::O_DIRECT).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let why = format!("{path} opened read-only was refused ({e}): `sudo osbench nvme` reads it");
                return super::BENCHES.iter().for_each(|b| unmeasured(b, &why, &[]));
            }
            Err(e) => return refuse("nvme", format!("{path}: {e}")),
        };
        // O_DIRECT reads into memory aligned to the logical block: a mapping is page-aligned.
        let mut buf = Mapping::huge(MIB);
        random(&file, bytes, &mut buf);
        sequential(&file, &mut buf);
    }

    /// 4 KiB at a page-aligned offset an LCG picks over the whole namespace,
    /// one at a time: 2,000 after 100.
    fn random(file: &File, bytes: u64, buf: &mut Mapping) {
        let pages = bytes / PAGE;
        let page = &mut buf.bytes()[..PAGE as usize];
        each(super::BENCHES[0], 100, 2000, &[("queue_depth", "1".into())], |i| {
            let at = ((i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 16) % pages * PAGE;
            let n = io("pread", file.read_at(page, at))?;
            if n == PAGE as usize { Ok(()) } else { Err(format!("read {n} bytes at {at}")) }
        });
    }

    /// 256 MiB in 1 MiB reads from where the last round ended: MB/s, three
    /// rounds after one.
    fn sequential(file: &File, buf: &mut Mapping) {
        let buf = buf.bytes();
        let mut at = 0u64;
        throughput(super::BENCHES[1], 3, SEQ_BYTES, &[("call_bytes", MIB.to_string()), ("queue_depth", "1".into())], || {
            let end = at + SEQ_BYTES;
            while at < end {
                let n = io("pread", file.read_at(buf, at))?;
                if n != MIB {
                    return Err(format!("read {n} bytes at {at}"));
                }
                at += MIB as u64;
            }
            Ok(())
        });
    }
}
