//! Two processes racing one page of one `/log` file: a write that lands while
//! a flush has that page copied but not yet marked clean must survive to the
//! device (F6). The parent dirties [`PAGES`] pages and fsyncs; the child lands
//! one 8-byte slot write in page 0 after a swept delay, so across [`ROUNDS`]
//! the write falls inside the copy-to-clear window many times. Each round
//! evicts page 0 (`test-small-caches` arms the pressure) and reads it back off
//! the device: every slot written so far must be there, and a cleared
//! mid-flush redirty loses exactly one slot forever.
//! `tests/common/volumes.rs::redirty_mid_flush` boots this and re-judges the
//! final bytes off the image.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PATH: &str = "/log/redirty.bin";
const JUNK: &str = "/log/redirty-junk.bin";
/// Width of each round's dirty set: page 0 is copied first and marked clean
/// last, so its exposure is the other pages' device writes.
const PAGES: usize = 12;
/// Mirrored in `tests/common/volumes.rs::redirty_mid_flush`, with `SLOTS_AT`.
const ROUNDS: u64 = 128;
/// The child's slot array in page 0; one slot per round, never rewritten, so a
/// lost slot stays lost. The parent's own stripe is at [`PARENT_AT`] per page.
const SLOTS_AT: u64 = 64;
const PARENT_AT: u64 = 2048;
const JUNK_PAGES: usize = 72;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        return child();
    }
    parent();
}

fn child() {
    let mut f = OpenOptions::new().write(true).open(PATH).expect("child open");
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.expect("child stdin");
        if line == "done" {
            return;
        }
        let mut parts = line.split(' ');
        let round: u64 = parts.next().expect("round").parse().expect("round");
        let delay_us: u64 = parts.next().expect("delay").parse().expect("delay");
        let start = Instant::now();
        while start.elapsed() < Duration::from_micros(delay_us) {
            std::hint::spin_loop();
        }
        f.seek(SeekFrom::Start(SLOTS_AT + round * 8)).expect("seek slot");
        f.write_all(&round.to_le_bytes()).expect("slot write");
        println!("did {round}");
    }
}

fn parent() {
    let mut f = File::create(PATH).expect("create");
    f.write_all(&vec![0u8; PAGES * 4096]).expect("fill");
    f.sync_all().expect("first fsync");
    {
        let mut junk = File::create(JUNK).expect("create junk");
        junk.write_all(&vec![0xEEu8; JUNK_PAGES * 4096]).expect("junk write");
        junk.sync_all().expect("junk fsync");
    }

    let mut child = Command::new("/system/bin/test_rs_redirty_mid_flush")
        .arg("child")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn child");
    let mut go = child.stdin.take().expect("child stdin");
    let mut ack = BufReader::new(child.stdout.take().expect("child stdout"));

    for round in 0..ROUNDS {
        for page in 0..PAGES {
            f.seek(SeekFrom::Start(page as u64 * 4096 + PARENT_AT)).expect("seek stripe");
            f.write_all(&round.to_le_bytes()).expect("stripe write");
        }
        writeln!(go, "{round} {}", (round * 131) % 5000).expect("send go");
        f.sync_all().expect("racing fsync");
        let mut line = String::new();
        ack.read_line(&mut line).expect("read ack");
        assert_eq!(line.trim(), format!("did {round}"), "child fell out of step");
        f.sync_all().expect("delivering fsync");
        evict();
        check_slots(round);
    }
    writeln!(go, "done").expect("send done");
    assert!(child.wait().expect("child wait").success(), "child failed");
    println!("{ROUNDS} racing rounds: every mid-flush write survived to the device");
}

/// Push page 0 out of the shrunken cache so the next read is the device's word.
fn evict() {
    let mut junk = File::open(JUNK).expect("open junk");
    let mut buf = vec![0u8; 4096];
    for _ in 0..2 {
        junk.seek(SeekFrom::Start(0)).expect("rewind junk");
        for _ in 0..JUNK_PAGES {
            junk.read_exact(&mut buf).expect("junk read");
        }
    }
}

fn check_slots(upto: u64) {
    let mut f = File::open(PATH).expect("re-open");
    f.seek(SeekFrom::Start(SLOTS_AT)).expect("seek slots");
    let mut raw = vec![0u8; (upto as usize + 1) * 8];
    f.read_exact(&mut raw).expect("slot read");
    for slot in 0..=upto {
        let got = u64::from_le_bytes(raw[slot as usize * 8..][..8].try_into().expect("8 bytes"));
        assert_eq!(
            got, slot,
            "slot {slot} reads {got} after eviction: a write that landed mid-flush was marked \
             clean and never reached the device"
        );
    }
}
