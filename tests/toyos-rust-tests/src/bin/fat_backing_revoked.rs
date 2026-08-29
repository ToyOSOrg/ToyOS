//! A FAT32 file backing must not outlive the file it reads.
//!
//! `/log` hands every open file a `FatBacking` holding the volume byte ranges
//! its data lives in. Unlink the file and `Fat32::remove` puts those clusters
//! back in the FAT — and FSInfo's `next_free` is walked *down* to the lowest one
//! freed, so the very next allocation on the volume is the one that takes them.
//! A backing that still names them reads that file's contents: an information
//! disclosure through `open`, `rm` and a write, with nothing crafted about it
//! and no privilege needed.
//!
//! The same defect `home_backing_revoked` covers for `/home`, on the other
//! filesystem and the other allocator. `FatFs::delete` already dropped the
//! *write* handle, so the destructive half was closed and the read half was not.
//!
//! Staged rather than reasoned about: the victim's clusters are freed and then
//! deliberately handed to a file whose bytes are nothing like the victim's, and
//! the still-open descriptor is read afterwards. The host half
//! (`tests/common/volumes.rs::fat_backing_revoked`) shuts the machine down and
//! reads the volume back with an independent FAT implementation and the
//! fatgen103 checker, so what the delete-and-reallocate cycle left on the stick
//! is judged by something that is not the kernel.

use std::fs;
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

/// Mirrored in `tests/common/volumes.rs::fat_backing_revoked`. Two halves of one
/// fixture; a change to either alone fails loudly rather than passing quietly.
const VICTIM: &str = "/log/fat-revoke-victim.bin";
const ATTACKER: &str = "/log/fat-revoke-attacker.bin";
const CONTROL: &str = "/log/fat-revoke-control.bin";

/// Eight pages. More than one so the read crosses pages, and — at the 512-byte
/// clusters a 34 MiB FAT32 volume gets — sixty-four clusters, so each page is
/// several extents and the multi-run half of `FatBacking::read_page` is the one
/// under test.
const LEN: usize = 8 * 4096;

const VICTIM_BYTE: u8 = 0xA7;
const ATTACKER_BYTE: u8 = 0x5C;

/// Five of the 2 s operation budgets behind the kernel's `WouldBlock` refusal
/// (`kernel/src/block.rs::OPERATION`): device patience is not what this test
/// is about, so its setup asks again the way logd's flush policy does.
const SETUP_PATIENCE: Duration = Duration::from_secs(10);
const SETUP_PAUSE: Duration = Duration::from_millis(200);

/// One idempotent setup step, asked again on `WouldBlock` until
/// [`SETUP_PATIENCE`] is spent; anything else panics with the step's message.
fn patient<T>(what: &str, mut op: impl FnMut() -> std::io::Result<T>) -> T {
    let start = Instant::now();
    loop {
        match op() {
            Ok(v) => return v,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                && start.elapsed() < SETUP_PATIENCE =>
            {
                thread::sleep(SETUP_PAUSE);
            }
            Err(e) => panic!("{what}: {e}"),
        }
    }
}

fn write_file(path: &str, byte: u8) {
    {
        let mut f = patient(&format!("create {path}"), || fs::File::create(path));
        // Not `patient`: a `write_all` refused partway has advanced the cursor,
        // so asking again blind would double bytes; it lands in the cache anyway.
        f.write_all(&vec![byte; LEN]).unwrap_or_else(|e| panic!("write {path}: {e}"));
        patient(&format!("fsync {path}"), || f.sync_all());
    } // close: the last handle drops here.
    // The last close no longer drops the file from the cache on this thread —
    // it pins it and hands the teardown to `iod` (`kernel::writeback`). Let that
    // drain, so a later open of this name is served by the backing rather than
    // adopting the pages this write left cached; the revocation this test checks
    // lives in the backing, and a cache-served read would never reach it. The
    // margin is enormous: the drain is microseconds of work.
    thread::sleep(Duration::from_millis(200));
}

fn read_all(f: &mut fs::File) -> std::io::Result<Vec<u8>> {
    let mut got = Vec::new();
    f.read_to_end(&mut got)?;
    Ok(got)
}

fn main() {
    // The control. `write_file` drains the write-back so the file leaves the
    // cache, so this open is served by the backing and not by pages the write
    // left cached — if it were not, the attack below would prove nothing about
    // that path.
    write_file(CONTROL, VICTIM_BYTE);
    let control = read_all(&mut fs::File::open(CONTROL).expect("open the control"))
        .expect("read the control");
    assert_eq!(control.len(), LEN, "the control read short");
    assert!(
        control.iter().all(|&b| b == VICTIM_BYTE),
        "the backing did not serve the control file's own bytes",
    );

    write_file(VICTIM, VICTIM_BYTE);

    // Held open, and deliberately not read: `write_file` drained the victim out
    // of the cache, so every page is absent and each one is a fault the backing
    // has to answer.
    let mut held = fs::File::open(VICTIM).expect("open the victim");

    fs::remove_file(VICTIM).expect("unlink the victim");

    // The victim's clusters are the lowest free ones now, so this takes them.
    write_file(ATTACKER, ATTACKER_BYTE);

    // **Refused, not zeroed.** A revoked backing has no bytes to serve and has
    // to say so; the byte checks below are kept for the case where the refusal
    // does not come, because a backing that still resolves serves either
    // {ATTACKER_BYTE:#04x} or {VICTIM_BYTE:#04x} and neither is zero.
    let refused = match read_all(&mut held) {
        Err(e) => e,
        Ok(got) => {
            if let Some(at) = got.iter().position(|&b| b == ATTACKER_BYTE) {
                panic!(
                    "byte {at} read through the deleted file's descriptor is \
                     {ATTACKER_BYTE:#04x} — the backing served another file's data",
                );
            }
            if let Some(at) = got.iter().position(|&b| b != 0) {
                panic!(
                    "byte {at} read through the deleted file's descriptor is {:#04x}, not \
                     zero — the backing still resolves clusters the FAT has taken back",
                    got[at],
                );
            }
            panic!(
                "the read through the deleted file's descriptor returned {} bytes and \
                 succeeded; a revoked backing has no bytes to serve and has to say so",
                got.len(),
            );
        }
    };

    // The name is gone as well as the bytes, and a fresh open says so with the
    // error a missing file gets rather than the one a revoked backing gets.
    assert!(fs::File::open(VICTIM).is_err(), "the unlinked victim still opens by name");

    // Left on the volume on purpose: the host reads both back off the image
    // with its own FAT implementation after the shutdown.
    println!(
        "a read through a backing whose file was deleted was refused ({refused}) rather than \
         serving any of the next file's {LEN} bytes"
    );
}
