//! blockd, driven from its client's side and supervised from this process.
//!
//! This binary holds the machine's second NVMe controller's claim the way init
//! holds a service's: it mints the claim, starts `/system/bin/blockd` holding
//! it and a port's acceptor, keeps a duplicate of the acceptor so the port
//! outlives any one blockd, and is the only thing that can end or restart it.
//! The disk is crafted, and the verdict on what reached it read back, by
//! `tests/common/blockd.rs` on the host.
//!
//! Roles, by the first argument:
//! - `claims` — the partition refusals by name, the idle ROOT slot written
//!   whole through a session and read back, and one holder at a time across
//!   two processes;
//! - `holder <expect>` — the second process: opens the slot and says what it
//!   was answered;
//! - `bench` — the same bytes through blockd, one request at a time and many;
//! - `hostile-head` — a client that, with a write on the device, moves its
//!   completion ring's head a ring behind blockd's tail: the session is
//!   ended, and blockd serves the next one;
//! - `reset` — blockd started withholding its second answer: the silence ends
//!   in a controller reset, the withheld write is answered not done, and the
//!   write acknowledged before it is on the medium after the next flush;
//! - `crash` — a FAT32 volume written through a session, blockd killed with a
//!   write on the wire, restarted, and the same volume carried on;
//! - `dma-inside`, `dma-outside`, `dma-revoked`, `dma-after` — the controller
//!   aimed by this process at a lent region, past it, and at one taken back;
//! - `dma-pool`, `dma-bound`, `dma-churn` — what a claim may lend: a kernel
//!   driver's pool refused, regions lent until the claim's bound refuses the
//!   next and a region no run of the window fits, and one region lent and taken
//!   back until ten domains' worth of addresses went by;
//! - `dma-residue` — on a boot where no release resets the function, three
//!   claims in turn, and none lends where the first one did.
//! - `nothing` — blockd started holding no claim: each first frame, malformed
//!   and well-formed, answered as `serve` answers it.

use std::io::{BufRead, BufReader, Write};
use std::os::toyos::process::{ChildExt, CommandExt};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};

use blockd::nvme::{Controller, Owner};
use blockd::region::Region;
use blockd::{Error, Outcome, Session, Unsent};
use toyos::endow::Endowments;
use toyos::poller::{Poller, READABLE};
use toyos::namespace::{self, Namespace};
use toyos::port::{self, Acceptor, Connector};
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::AsHandle;
use toyos_abi::part::PartGuid;
use toyos_abi::syscall::{self, DeviceType, PciId, SpawnArgs, SyscallError, DEV_PREFIX, SERVE_PREFIX, SYSCAP_LABEL};
use toyos_blockring::layout::{ARENA, CQ_HEAD, CQ_TAIL, DEPTH, SQ_BASE, SQ_TAIL};
use toyos_blockring::wire::{self, Refusal};
use toyos_blockring::{Op, Request, BLOCK_BYTES, MAX_REQUEST_BLOCKS, PORT};

const SELF: &str = "/system/bin/test_rs_blockd_io";

/// Mirrored in `tests/common/blockd.rs`: the controller blockd drives, QEMU's
/// NVMe under Intel's ids so a claim names it apart from the kernel's.
const BLOCKD: PciId = PciId { vendor: 0x8086, device: 0x5845 };
/// Mirrored: blockd's disk.
const TARGET: &str = "9C4E2A71-5B3D-4F18-A6E0-2D7C8B1F3E59";
const FS: &str = "B2D4F6A8-1C3E-4A57-9B0D-E2F4A6C8E0A1";
const BENCH: &str = "C3E5A7B9-2D4F-4B68-8C1E-F3A5B7D9F1B2";
const MISALIGNED: &str = "E5A7C9DB-4F6B-4D8A-8E30-B5C7D9FB13D4";
const MISSTART: &str = "F6B8DAEC-5A7C-4E9B-9F41-C6D8EA0C24E5";
const ABSENT: &str = "0A1B2C3D-4E5F-4A6B-8C7D-9E0F1A2B3C4D";
/// Mirrored: the idle slot's length in blocks, and what each block holds.
const TARGET_BLOCKS: u64 = 2048;
/// Mirrored: what the bench moves each way, each side.
const BENCH_BLOCKS: u64 = 8192;
/// Mirrored: the files the crash role writes before it kills blockd, and the
/// bytes each holds.
const FILES: usize = 6;
const FILE_BYTES: usize = 48 * 1024;
/// Mirrored: the file written after the restart, on the same mount.
const AFTER: &str = "/AFTER.BIN";

/// Mirrored in `tests/common/blockd.rs`: the most a claim may hold across its
/// grants and what it lends (`pcidev::MAX_GRANT_TOTAL`), one region, and the
/// addresses a device domain has under `iommu-domain-narrow`
/// (`vtd::table::NARROW_BYTES`).
const GRANT_TOTAL: u64 = 32 * 1024 * 1024;
const REGION: usize = 2 * 1024 * 1024;
const NARROW: u64 = 128 * 1024 * 1024;

fn guid(text: &str) -> [u8; 16] {
    PartGuid::parse(text).unwrap_or_else(|| panic!("{text} is no GUID")).0
}

/// Mirrored: block `n` of a region `salt` names.
fn pattern(salt: u8, n: u64) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK_BYTES];
    for (i, byte) in block.iter_mut().enumerate() {
        *byte = (n as usize).wrapping_mul(131).wrapping_add(i).wrapping_add(salt as usize) as u8;
    }
    block[..8].copy_from_slice(&n.to_le_bytes());
    block[8] = salt;
    block[9..24].copy_from_slice(b"TOYOS-BLOCKDIO\0");
    block
}

fn fail(what: String) -> ! {
    println!("blockd_io: FAIL {what}");
    let _ = std::io::stdout().flush();
    std::process::exit(1)
}

/// blockd, held by this process: its claim minted here, from `syscap` when
/// there is one, the port's acceptor kept here, so a blockd can end and
/// another take its place on the same name. What blockd says goes to this
/// process's stdout, a line at a time.
struct Blockd {
    syscap: Option<SysCap>,
    acceptor: Acceptor,
    connector: Connector,
    child: Option<Child>,
    /// Every line the running blockd says.
    said: Option<Receiver<String>>,
}

impl Blockd {
    fn start(args: &[&str]) -> Self {
        Self::with(Some(capability()), args)
    }

    fn with(syscap: Option<SysCap>, args: &[&str]) -> Self {
        let (acceptor, connector) = port::create().unwrap_or_else(|e| fail(format!("no port: {e:?}")));
        let mut blockd = Self { syscap, acceptor, connector, child: None, said: None };
        blockd.spawn(args, false);
        blockd
    }

    fn names(&self) -> Namespace {
        namespace::build().add(PORT, &self.connector).finish().unwrap_or_else(|e| fail(format!("no namespace: {e:?}")))
    }

    /// Mint the claim — waiting out the one the last blockd held — and start
    /// a blockd holding it and a duplicate of the acceptor. With
    /// `kill_on_withheld`, blockd is killed the moment it says it withheld a
    /// write's answer: the write is done on the device, and its session never
    /// hears.
    fn spawn(&mut self, args: &[&str], kill_on_withheld: bool) {
        let mut command = Command::new("/system/bin/blockd");
        if let Some(syscap) = &self.syscap {
            let claim: toyos::Device = claim_when_free(syscap);
            command.endow(&format!("{DEV_PREFIX}pci:8086:5845"), claim.into_raw().0);
        }
        let acceptor = toyos_abi::syscall::dup(self.acceptor.as_handle())
            .unwrap_or_else(|e| fail(format!("the acceptor would not duplicate: {e:?}")));
        command.args(args);
        command.stdout(Stdio::piped());
        command.endow(&format!("{SERVE_PREFIX}{PORT}"), acceptor.0);
        let mut child = command.spawn().unwrap_or_else(|e| fail(format!("blockd did not start: {e}")));
        let out = child.stdout.take().expect("piped");
        let (says, said) = mpsc::channel();
        let mut kill = kill_on_withheld.then(|| {
            toyos_abi::syscall::dup(toyos_abi::RawHandle(child.as_raw_handle()))
                .unwrap_or_else(|e| fail(format!("blockd's handle would not duplicate: {e:?}")))
        });
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                println!("{line}");
                if line.contains("WITHHELD") {
                    if let Some(handle) = kill.take() {
                        let _ = toyos_abi::syscall::process_kill(handle);
                        println!("blockd_io: blockd killed with the withheld write done on the device");
                    }
                }
                let _ = says.send(line);
            }
        });
        self.child = Some(child);
        self.said = Some(said);
    }

    /// Wait, with no deadline, for the running blockd to say a line holding
    /// `needle`.
    fn says(&self, needle: &str) {
        let said = self.said.as_ref().expect("spawned");
        loop {
            match said.recv() {
                Ok(line) if line.contains(needle) => return,
                Ok(_) => {}
                Err(_) => fail(format!("blockd ended before it said {needle:?}")),
            }
        }
    }

    /// End the running blockd, if one is, and wait for it to be gone.
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Blockd {
    fn drop(&mut self) {
        self.kill();
    }
}

/// This process's capability, which test-runner endowed.
fn capability() -> SysCap {
    Endowments::get().take(SYSCAP_LABEL).unwrap_or_else(|| fail("started with no system capability".into()))
}

fn open(names: Namespace, text: &str) -> Session {
    Session::open(names, PORT, guid(text)).unwrap_or_else(|e| fail(format!("{text} did not open: {e:?}")))
}

/// `blocks` blocks of `salt`'s pattern from block 0, cut into requests of the
/// largest size one may be: built before anything is timed.
fn chunks(blocks: u64, salt: u8) -> Vec<Vec<u8>> {
    let per = MAX_REQUEST_BLOCKS as u64;
    (0..blocks.div_ceil(per))
        .map(|c| (c * per..(c * per + per).min(blocks)).flat_map(|b| pattern(salt, b)).collect())
        .collect()
}

/// Write `chunks` from block 0, `in_flight` requests at a time; flush. Answers
/// how many flushes there were: an acknowledged write holds its arena blocks
/// until a flush covers it, so a full arena is where one is asked.
fn write_all(s: &mut Session, chunks: &[Vec<u8>], in_flight: usize) -> u32 {
    let mut next = 0usize;
    let mut lba = 0u64;
    let mut outstanding = 0usize;
    let mut full = false;
    let mut flushes = 0u32;
    while next < chunks.len() || outstanding > 0 {
        while next < chunks.len() && outstanding < in_flight && !full {
            match s.submit_write(lba, &chunks[next]) {
                Ok(_) => {
                    lba += (chunks[next].len() / BLOCK_BYTES) as u64;
                    next += 1;
                    outstanding += 1;
                }
                Err(Unsent::ArenaFull) => full = true,
                Err(Unsent::Ended) => fail("blockd ended under a write".into()),
            }
        }
        if outstanding > 0 {
            let waited = s.wait(true);
            if waited.ended {
                fail("blockd ended under a write".into());
            }
            for answer in waited.answers {
                if answer.outcome != Outcome::Done {
                    fail(format!("a write was answered {:?}", answer.outcome));
                }
                outstanding -= 1;
            }
        }
        if full && outstanding == 0 {
            flushed(s);
            flushes += 1;
            full = false;
        }
    }
    flushed(s);
    flushes + 1
}

fn flushed(s: &mut Session) {
    match s.flush() {
        Ok(Outcome::Durable) => {}
        other => fail(format!("a flush was answered {other:?}")),
    }
}

/// Read `blocks` from block 0, `in_flight` requests at a time; the blocks, in
/// order.
fn read_all(s: &mut Session, blocks: u64, in_flight: usize) -> Vec<u8> {
    let per = MAX_REQUEST_BLOCKS as u64;
    let mut out = vec![0u8; (blocks * BLOCK_BYTES as u64) as usize];
    let mut next = 0u64;
    let mut asked: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
    while next < blocks || !asked.is_empty() {
        while next < blocks && asked.len() < in_flight {
            let n = per.min(blocks - next);
            match s.submit_read(next, n as u32) {
                Ok(ticket) => {
                    asked.insert(ticket, next);
                    next += n;
                }
                Err(_) => break,
            }
        }
        let waited = s.wait(true);
        if waited.ended {
            fail("blockd ended under a read".into());
        }
        for answer in waited.answers {
            let first = asked.remove(&answer.ticket).expect("a ticket this asked for");
            let data = match (answer.outcome, answer.data) {
                (Outcome::Done, Some(data)) => data,
                (outcome, _) => fail(format!("a read at {first} was answered {outcome:?}")),
            };
            let at = (first * BLOCK_BYTES as u64) as usize;
            out[at..at + data.len()].copy_from_slice(&data);
        }
    }
    out
}

/// `read` holds `chunks`, block for block.
fn holds(read: &[u8], chunks: &[Vec<u8>], what: &str) {
    let mut at = 0usize;
    for (c, chunk) in chunks.iter().enumerate() {
        if read[at..at + chunk.len()] != chunk[..] {
            fail(format!("{what}: request {c}'s blocks read back are not what was written"));
        }
        at += chunk.len();
    }
}

fn claims() {
    let blockd = Blockd::start(&[]);
    for (what, text, want) in [
        ("an absent GUID", ABSENT, Refusal::NotFound),
        ("the zero GUID", "00000000-0000-0000-0000-000000000000", Refusal::NotFound),
        ("a partition not whole blocks long", MISALIGNED, Refusal::Unusable),
        ("a partition beginning inside a block", MISSTART, Refusal::Unusable),
    ] {
        match Session::open(blockd.names(), PORT, guid(text)) {
            Err(Error::Refused(got)) if got == want => {
                println!("blockd_io: {what} refused with {got:?}")
            }
            Err(e) => fail(format!("{what} was answered {e:?}, not {want:?}")),
            Ok(_) => fail(format!("{what} opened")),
        }
    }
    oversized(&blockd);
    let mut slot = open(blockd.names(), TARGET);
    if slot.blocks() != TARGET_BLOCKS {
        fail(format!("the slot is {} blocks, not {TARGET_BLOCKS}", slot.blocks()));
    }
    holder(&blockd, "Held");
    let written = chunks(TARGET_BLOCKS, 0x5A);
    write_all(&mut slot, &written, 8);
    holds(&read_all(&mut slot, TARGET_BLOCKS, 8), &written, "the idle slot");
    println!(
        "blockd_io: the idle slot's {TARGET_BLOCKS} blocks written and read back; at most {} \
         requests on the wire",
        slot.peak_on_the_wire()
    );
    drop(slot);
    holder(&blockd, "Opened");
    println!("blockd_io: PASS claims");
}

/// An open sending a region longer than a session is refused, and costs the
/// claim nothing: the slot opens after it.
fn oversized(blockd: &Blockd) {
    let conn = blockd.names().open(PORT).unwrap_or_else(|e| fail(format!("the port: {e:?}")));
    let region = SharedMemory::create(2 * REGION).unwrap_or_else(|e| fail(format!("a region: {e:?}")));
    let shared = region.share().unwrap_or_else(|e| fail(format!("a second handle: {e:?}")));
    conn.send_bytes_with_handles(&[shared], wire::MSG_OPEN, &guid(TARGET))
        .unwrap_or_else(|e| fail(format!("the open: {e:?}")));
    let header = conn.recv_header().unwrap_or_else(|e| fail(format!("the answer: {e:?}")));
    let mut payload = [0u8; 64];
    let len = conn.recv_bytes(&header, &mut payload).unwrap_or_else(|e| fail(format!("the answer: {e:?}")));
    match (header.msg_type, Refusal::decode(&payload[..len])) {
        (wire::MSG_REFUSED, Some(Refusal::Malformed)) => {}
        (msg_type, refusal) => fail(format!("a {}-byte region was answered {msg_type} {refusal:?}", 2 * REGION)),
    }
    println!("blockd_io: a region of {} bytes, longer than a session, refused with Malformed", 2 * REGION);
}

/// Another process opens the slot through the same port, and must be answered
/// `expect`.
fn holder(blockd: &Blockd, expect: &str) {
    let names = blockd.names();
    let mut command = Command::new(SELF);
    command.args(["holder", expect]);
    command.endow("block-ns", names.into_raw().0);
    let status = command
        .spawn()
        .and_then(|mut c| c.wait())
        .unwrap_or_else(|e| fail(format!("the second client did not run: {e}")));
    if status.code() != Some(0) {
        fail(format!("the second client, expecting {expect}, exited {status:?}"));
    }
}

fn holder_role(expect: &str) {
    let names: Namespace = Endowments::get().take("block-ns").unwrap_or_else(|| fail("no namespace".into()));
    let got = match Session::open(names, PORT, guid(TARGET)) {
        Ok(_) => "Opened".to_string(),
        Err(Error::Refused(r)) => format!("{r:?}"),
        Err(e) => format!("{e:?}"),
    };
    if got != expect {
        fail(format!("a second client was answered {got}, not {expect}"));
    }
    println!("blockd_io: a second client of the slot refused with {got}, as expected");
}

/// The same bytes through blockd, one request at a time and then as many as
/// the arena holds.
fn bench() {
    let blockd = Blockd::start(&[]);
    let mut s = open(blockd.names(), BENCH);
    let mut runs = Vec::new();
    for (salt, in_flight) in [(0x3D, 1usize), (0x3C, 15)] {
        let written = chunks(BENCH_BLOCKS, salt);
        let flushes = write_all(&mut s, &written, in_flight);
        let read = read_all(&mut s, BENCH_BLOCKS, in_flight);
        holds(&read, &written, "blockd's bench partition");
        runs.push(format!("{in_flight} in flight with {flushes} Flushes"));
    }
    println!(
        "blockd_io: bench {} MiB each way through blockd {}; at most {} requests on the wire",
        BENCH_BLOCKS * BLOCK_BYTES as u64 / (1024 * 1024),
        runs.join("; "),
        s.peak_on_the_wire()
    );
    println!("blockd_io: PASS bench");
}

fn reset() {
    let blockd = Blockd::start(&["--silence-write", "2"]);
    let mut s = open(blockd.names(), BENCH);
    let first = pattern(0x71, 0);
    match s.write(0, &first) {
        Ok(Outcome::Done) => {}
        other => fail(format!("the first write was answered {other:?}")),
    }
    match s.write(1, &pattern(0x71, 1)) {
        Ok(Outcome::Device) => {}
        other => fail(format!("the withheld write was answered {other:?}, not Device")),
    }
    println!("blockd_io: the withheld write was answered Device");
    // The reset may have dropped the device's cache: the write acknowledged
    // before it goes out again, inside this flush.
    flushed(&mut s);
    println!(
        "blockd_io: {} acknowledged writes no flush had covered went out again after the reset",
        s.reissued()
    );
    match s.read(0, 1) {
        Ok((Outcome::Done, Some(data))) if data == first => {}
        other => fail(format!("block 0 read back after the reset: {:?}", other.map(|(o, _)| o))),
    }
    println!("blockd_io: PASS reset");
}

/// A client whose write is on the device moves its completion ring's head a
/// ring's depth behind the tail blockd published, so the answer finds no room:
/// blockd ends that session, and serves the next.
fn hostile_head() {
    let blockd = Blockd::start(&["--silence-write", "1"]);
    let region = Region::create().unwrap_or_else(|e| fail(format!("a region: {e:?}")));
    let conn = blockd.names().open(PORT).unwrap_or_else(|e| fail(format!("the port: {e:?}")));
    let shared = region.share().unwrap_or_else(|e| fail(format!("a second handle: {e:?}")));
    conn.send_bytes_with_handles(&[shared], wire::MSG_OPEN, &guid(TARGET))
        .unwrap_or_else(|e| fail(format!("the open: {e:?}")));
    let header = conn.recv_header().unwrap_or_else(|e| fail(format!("the answer: {e:?}")));
    let mut payload = [0u8; 64];
    conn.recv_bytes(&header, &mut payload).unwrap_or_else(|e| fail(format!("the answer: {e:?}")));
    if header.msg_type != wire::MSG_OPENED {
        fail(format!("the slot's open was answered {}", header.msg_type));
    }
    // A write of the slot's block 0 from arena block 0 under tag 1, as the
    // words a client puts on the request ring, published and rung.
    let words = region.words();
    let run = ARENA.run(0, 1).unwrap_or_else(|| fail("arena block 0 is no run".into()));
    let write = Request { op: Op::Write { run, lba: 0 }, tag: 1 };
    for (at, word) in write.encode().into_iter().enumerate() {
        words[SQ_BASE + at].store(word, Ordering::Relaxed);
    }
    words[SQ_TAIL].store(1, Ordering::Release);
    conn.write_nonblock(&[1]).unwrap_or_else(|e| fail(format!("the doorbell: {e:?}")));
    blockd.says("WITHHELD");
    let tail = words[CQ_TAIL].load(Ordering::Acquire);
    words[CQ_HEAD].store(tail.wrapping_sub(DEPTH), Ordering::Release);
    conn.write_nonblock(&[1]).unwrap_or_else(|e| fail(format!("the doorbell: {e:?}")));
    println!("blockd_io: with a write on the device, the client moved its completion head {DEPTH} behind the tail");
    blockd.says("closed after");
    println!("blockd_io: blockd ended the session and runs on");
    let mut next = open(blockd.names(), TARGET);
    let block = pattern(0x6B, 0);
    match next.write(0, &block) {
        Ok(Outcome::Done) => {}
        other => fail(format!("the next session's write was answered {other:?}")),
    }
    flushed(&mut next);
    match next.read(0, 1) {
        Ok((Outcome::Done, Some(data))) if data == block => {}
        other => fail(format!("the next session read back {:?}", other.map(|(o, _)| o))),
    }
    println!("blockd_io: the next session wrote, flushed and read back the slot's block 0");
    println!("blockd_io: PASS hostile-head");
}

/// The FAT32 volume's device: a session, with blockd's supervisor beside it.
struct Volume {
    blockd: Blockd,
    session: Session,
    /// What a write that was not done was answered, the last time one was.
    refused: Option<Outcome>,
}

impl Volume {
    fn read_block(&mut self, lba: u64) -> Result<Vec<u8>, toyos_fat32::IoError> {
        match self.session.read(lba, 1) {
            Ok((Outcome::Done, Some(data))) => Ok(data),
            _ => Err(toyos_fat32::IoError::Device),
        }
    }

    fn write_block(&mut self, lba: u64, data: &[u8]) -> Result<(), toyos_fat32::IoError> {
        match self.session.write(lba, data) {
            Ok(Outcome::Done) => Ok(()),
            Ok(outcome) => {
                self.refused = Some(outcome);
                Err(toyos_fat32::IoError::Device)
            }
            Err(_) => Err(toyos_fat32::IoError::Device),
        }
    }

    /// The session has ended: wait for it to say so, start a blockd in the
    /// last one's place, and reopen.
    fn restart(&mut self, args: &[&str], kill_on_withheld: bool) {
        while !self.session.wait(true).ended {}
        self.blockd.kill();
        self.blockd.spawn(args, kill_on_withheld);
        self.session.reconnect().unwrap_or_else(|e| fail(format!("the session did not reopen: {e:?}")));
    }
}

impl toyos_fat32::BlockAccess for Volume {
    fn capacity(&self) -> u64 {
        self.session.blocks() * BLOCK_BYTES as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), toyos_fat32::IoError> {
        let mut done = 0usize;
        while done < buf.len() {
            let at = offset + done as u64;
            let lba = at / BLOCK_BYTES as u64;
            let within = (at % BLOCK_BYTES as u64) as usize;
            let n = (BLOCK_BYTES - within).min(buf.len() - done);
            let block = self.read_block(lba)?;
            buf[done..done + n].copy_from_slice(&block[within..within + n]);
            done += n;
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), toyos_fat32::IoError> {
        let mut done = 0usize;
        while done < buf.len() {
            let at = offset + done as u64;
            let lba = at / BLOCK_BYTES as u64;
            let within = (at % BLOCK_BYTES as u64) as usize;
            let n = (BLOCK_BYTES - within).min(buf.len() - done);
            let mut block = if n == BLOCK_BYTES { vec![0u8; BLOCK_BYTES] } else { self.read_block(lba)? };
            block[within..within + n].copy_from_slice(&buf[done..done + n]);
            self.write_block(lba, &block)?;
            done += n;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), toyos_fat32::IoError> {
        match self.session.flush() {
            Ok(Outcome::Durable) => Ok(()),
            _ => Err(toyos_fat32::IoError::Device),
        }
    }
}

/// Mirrored: file `i`'s bytes.
fn file_bytes(i: usize) -> Vec<u8> {
    (0..FILE_BYTES).map(|b| (b.wrapping_mul(7) ^ i.wrapping_mul(0x3D)) as u8).collect()
}

fn file_name(i: usize) -> String {
    format!("/F{i}.BIN")
}

fn write_file(fs: &mut toyos_fat32::Fat32<Volume>, name: &str, bytes: &[u8]) -> Result<(), toyos_fat32::Error> {
    let mut f = fs.create(name, toyos_fat32::FatTime::EPOCH)?;
    fs.write(&mut f, 0, bytes)?;
    fs.flush_meta(&mut f, toyos_fat32::FatTime::EPOCH)?;
    fs.sync()
}

fn read_file(fs: &mut toyos_fat32::Fat32<Volume>, name: &str) -> Result<Vec<u8>, toyos_fat32::Error> {
    let mut f = fs.open(name)?;
    let mut buf = vec![0u8; f.len() as usize];
    let n = fs.read(&mut f, 0, &mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

fn crash() {
    let blockd = Blockd::start(&[]);
    let session = open(blockd.names(), FS);
    let volume = Volume { blockd, session, refused: None };
    let mut fs = toyos_fat32::Fat32::mount(volume).unwrap_or_else(|e| fail(format!("FS did not mount: {e:?}")));
    for i in 0..FILES {
        write_file(&mut fs, &file_name(i), &file_bytes(i))
            .unwrap_or_else(|e| fail(format!("{} was not written: {e:?}", file_name(i))));
    }
    println!("blockd_io: {FILES} files written and flushed");

    // A blockd that will do the third write it is asked for and never say so,
    // and is killed the moment it has: two writes acknowledged and not yet
    // flushed, one done on the device and refused, when it dies.
    let v = fs.device();
    v.blockd.kill();
    v.restart(&["--silence-write", "3"], true);
    let doomed = file_name(FILES);
    match write_file(&mut fs, &doomed, &file_bytes(FILES)) {
        Err(e) => println!("blockd_io: {doomed} refused when blockd died under it: {e:?}"),
        Ok(()) => fail(format!("{doomed} was written with blockd killed under it")),
    }
    match fs.device().refused {
        Some(Outcome::Refused) => {
            println!("blockd_io: the write the device did and blockd died before answering was answered Refused")
        }
        other => fail(format!("the write on the wire when blockd died was answered {other:?}")),
    }

    // A new blockd on the same port, the same session reopened over the same
    // region: the two writes the old one acknowledged and no flush covered go
    // out again first.
    fs.device().restart(&[], false);
    println!("blockd_io: blockd restarted and the session reopened");

    // The same mount carries on: its first mutating call re-drives the repair
    // the refused write left, so the volume is whole again before anything new
    // lands on it.
    write_file(&mut fs, AFTER, &file_bytes(99)).unwrap_or_else(|e| fail(format!("{AFTER} after the restart: {e:?}")));
    println!(
        "blockd_io: {} acknowledged writes no flush had covered went out again after the restart",
        fs.device().session.reissued()
    );
    for i in 0..FILES {
        let got = read_file(&mut fs, &file_name(i)).unwrap_or_else(|e| fail(format!("{}: {e:?}", file_name(i))));
        if got != file_bytes(i) {
            fail(format!("{} does not read back after the restart", file_name(i)));
        }
    }
    // And a mount that saw nothing of the crash reads the same.
    let volume = fs.into_device();
    let mut fresh = toyos_fat32::Fat32::mount(volume).unwrap_or_else(|e| fail(format!("remount: {e:?}")));
    for (name, want) in (0..FILES).map(|i| (file_name(i), file_bytes(i))).chain([(AFTER.to_string(), file_bytes(99))]) {
        let got = read_file(&mut fresh, &name).unwrap_or_else(|e| fail(format!("{name} on a fresh mount: {e:?}")));
        if got != want {
            fail(format!("{name} does not read back on a fresh mount"));
        }
    }
    println!("blockd_io: every acknowledged file, and one written after the restart, reads back on a fresh mount");
    println!("blockd_io: PASS crash");
}

/// A controller this process drives itself, and a region to lend it filled
/// with a sentinel.
fn aim() -> (Controller, SharedMemory) {
    let dev: toyos::PciDev = capability().claim_pci(BLOCKD).unwrap_or_else(|e| fail(format!("claim: {e:?}")));
    let ctrl = Controller::open(dev, None).unwrap_or_else(|e| fail(format!("the controller: {e}")));
    let mut region = SharedMemory::create(2 * 1024 * 1024).unwrap_or_else(|e| fail(format!("a region: {e:?}")));
    region.as_mut_slice().fill(0xA5);
    (ctrl, region)
}

/// A spawn from `image`'s first `len` bytes, with an argv no process can read.
fn spawn_unreadable_argv(image: toyos::RawHandle, len: u64) -> Result<toyos::RawHandle, SyscallError> {
    // SAFETY: argv names the null page, which the kernel refuses to read, and
    // every other pointer is null with a zero length.
    unsafe {
        syscall::spawn(&SpawnArgs {
            argv_ptr: 8,
            argv_len: 8,
            slot_map_ptr: 0,
            slot_map_count: 0,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: 0,
            endow_count: 0,
            labels_ptr: 0,
            labels_len: 0,
            cwd_ptr: 0,
            cwd_len: 0,
            image: image.0 as u64,
            image_len: len,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
}

/// Device block 0, the disk's protective MBR, ends 0x55 0xAA.
fn is_block_zero(bytes: &[u8]) -> bool {
    bytes[510] == 0x55 && bytes[511] == 0xAA
}

fn dma(role: &str) {
    let (mut ctrl, region) = aim();
    let mapping = ctrl.claim().dma_map(region.as_handle()).unwrap_or_else(|e| fail(format!("dma_map: {e:?}")));
    println!("blockd_io: region lent at device address {:#x}, {} bytes", mapping.device_addr, mapping.bytes);
    match role {
        "dma-inside" | "dma-after" => {
            match transfer(&mut ctrl, 0, mapping.device_addr) {
                Ok(true) => {}
                other => fail(format!("a read into the lent region was answered {other:?}")),
            }
            if !is_block_zero(region.as_slice()) {
                fail("the lent region does not hold device block 0".into());
            }
            println!("blockd_io: the device read block 0 into the lent region");
            // Only memory the kernel allocated is lent, and once: the
            // function's own register window lent to it would aim the device
            // at a device, and a region lent twice is two grants of one page.
            let bar = ctrl.claim().map_bar(0, 4096).unwrap_or_else(|e| fail(format!("the BAR: {e:?}")));
            match ctrl.claim().dma_map(bar.as_handle()) {
                Err(SyscallError::InvalidArgument) => {}
                other => fail(format!("lending the register window was answered {other:?}")),
            }
            match ctrl.claim().dma_map(region.as_handle()) {
                Err(SyscallError::InvalidArgument) => {}
                other => fail(format!("lending the region a second time was answered {other:?}")),
            }
            println!("blockd_io: a register window, and a region already lent, are refused with InvalidArgument");
            // Nor is a register window a program: the spawn refuses it before
            // it reads anything else, where a region's is taken and the spawn
            // goes on to refuse the argv.
            match spawn_unreadable_argv(bar.as_handle(), 4096) {
                Err(SyscallError::InvalidArgument) => {}
                other => fail(format!("a spawn from the register window was answered {other:?}")),
            }
            match spawn_unreadable_argv(region.as_handle(), 4096) {
                Err(SyscallError::BadAddress) => {}
                other => fail(format!("a spawn from the region with an unreadable argv was answered {other:?}")),
            }
            println!("blockd_io: a spawn from a register window is refused with InvalidArgument, and one from a region reaches its argv");
        }
        "dma-outside" => {
            let past = mapping.device_addr + mapping.bytes;
            println!("blockd_io: aiming the device at {past:#x}, the first address past the lent region");
            refused(&mut ctrl, &region, past, "past the lent region");
        }
        "dma-revoked" => {
            ctrl.claim().dma_unmap(mapping.device_addr).unwrap_or_else(|e| fail(format!("dma_unmap: {e:?}")));
            println!(
                "blockd_io: aiming the device at {:#x}, where the region was lent until it was taken back",
                mapping.device_addr
            );
            refused(&mut ctrl, &region, mapping.device_addr, "at the region taken back");
        }
        _ => unreachable!(),
    }
    println!("blockd_io: PASS {role}");
}

/// A read aimed at `at`, which the function's domain does not map: the unit
/// refuses it, the claim answers the refusal from then on, and `region` —
/// still this process's — holds its sentinel.
///
/// **What the device answers is not the verdict**: QEMU's NVMe completes the
/// command with success when the unit drops its data, and the completion can
/// land before the fault takes the function off the bus. The verdict is the
/// unit's, read three ways: the claim's refusal here, the region untouched
/// here, and the fault record the host reads at `at`.
fn refused(ctrl: &mut Controller, region: &SharedMemory, at: u64, what: &str) {
    let answered = transfer(ctrl, 0, at);
    println!("blockd_io: the device answered a read aimed {what} with {answered:?}");
    await_refusal(ctrl);
    if !region.as_slice().iter().all(|b| *b == 0xA5) {
        fail(format!("a read aimed {what} changed the lent region"));
    }
    println!(
        "blockd_io: the unit refused a read aimed {what}, the claim answers the refusal, and the \
         region is untouched"
    );
}

/// One read of device block `block` into device address `at`, waited for with
/// no deadline on the claim's interrupt, with nothing else on the device:
/// whether the device did it, or the claim's refusal once the unit refused the
/// function an access — which is how a read aimed outside the function's
/// domain ends.
fn transfer(ctrl: &mut Controller, block: u64, at: u64) -> Result<bool, SyscallError> {
    if ctrl.busy() != 0 {
        fail("a waited read beside other commands".into());
    }
    ctrl.submit_io(false, block, 1, at, Owner::Driver);
    let poller = Poller::new(1);
    let mut done = Vec::new();
    loop {
        ctrl.reap(&mut done);
        if let Some(d) = done.pop() {
            return Ok(d.ok);
        }
        poller.watch(ctrl.claim(), READABLE, 0);
        poller.wait(1, u64::MAX, |_| {});
        ctrl.take_interrupt()?;
    }
}

/// Wait, with no deadline, for the claim to answer with the unit's refusal —
/// what every call on a claim answers once its function was refused an access.
/// A unit that never refuses leaves this waiting, and the harness ceiling reds
/// it.
fn await_refusal(ctrl: &Controller) {
    let poller = Poller::new(1);
    while ctrl.take_interrupt() != Err(SyscallError::Io) {
        poller.watch(ctrl.claim(), READABLE, 0);
        poller.wait(1, u64::MAX, |_| {});
    }
}

/// A kernel driver's own pool is not the holder's to lend, though it is
/// ordinary memory the holder may map: virtio-sound's, claimed here since no
/// soundd runs on this boot.
fn dma_pool() {
    let syscap = capability();
    let sound: toyos::VirtioSoundDev = syscap
        .claim(DeviceType::VirtioSound)
        .unwrap_or_else(|e| fail(format!("virtio-sound's claim: {e:?}")));
    let info = sound.info().unwrap_or_else(|e| fail(format!("virtio-sound's description: {e:?}")));
    let dev: toyos::PciDev = syscap.claim_pci(BLOCKD).unwrap_or_else(|e| fail(format!("claim: {e:?}")));
    match dev.dma_map(info.dma) {
        Err(SyscallError::InvalidArgument) => {}
        other => fail(format!("lending virtio-sound's pool was answered {other:?}")),
    }
    println!("blockd_io: virtio-sound's pool, a kernel driver's own, is refused with InvalidArgument");
    println!("blockd_io: PASS dma-pool");
}

/// Regions lent beside the claim's own grant until the claim's bound refuses
/// the next, and exactly as many as the bound leaves room for.
fn dma_bound() {
    let dev: toyos::PciDev = capability().claim_pci(BLOCKD).unwrap_or_else(|e| fail(format!("claim: {e:?}")));
    let grant = dev.dma_alloc(REGION as u64).unwrap_or_else(|e| fail(format!("the claim's grant: {e:?}")));
    match dev.dma_unmap(grant.device_addr) {
        Err(SyscallError::NotFound) => {}
        other => fail(format!("taking the claim's own grant back as a lent region was answered {other:?}")),
    }
    println!("blockd_io: the claim's own grant is not taken back as a lent region: NotFound");
    let room = ((GRANT_TOTAL - REGION as u64) / REGION as u64) as usize;
    let mut lent = Vec::new();
    let refused = loop {
        let region = SharedMemory::create(REGION).unwrap_or_else(|e| fail(format!("a region: {e:?}")));
        match dev.dma_map(region.as_handle()) {
            Ok(mapping) if mapping.bytes == REGION as u64 => lent.push((region, mapping)),
            Ok(mapping) => fail(format!("a {REGION}-byte region was lent as {} bytes", mapping.bytes)),
            Err(why) => break why,
        }
        if lent.len() > room + 1 {
            fail(format!("{} regions lent past a bound that has room for {room}", lent.len()));
        }
    };
    if refused != SyscallError::ResourceExhausted || lent.len() != room {
        fail(format!("{} regions lent and the next answered {refused:?}, not {room} and ResourceExhausted", lent.len()));
    }
    // One taken back is room for one more, and no more than one.
    let (_, first) = lent.remove(0);
    dev.dma_unmap(first.device_addr).unwrap_or_else(|e| fail(format!("dma_unmap: {e:?}")));
    for (n, want) in [(1, true), (2, false)] {
        let region = SharedMemory::create(REGION).unwrap_or_else(|e| fail(format!("a region: {e:?}")));
        match (dev.dma_map(region.as_handle()), want) {
            (Ok(mapping), true) => lent.push((region, mapping)),
            (Err(SyscallError::ResourceExhausted), false) => {}
            (other, _) => fail(format!("lend {n} after one was taken back was answered {other:?}")),
        }
    }
    println!(
        "blockd_io: {room} regions of {REGION} bytes lent beside the claim's own grant, the next refused \
         with ResourceExhausted, and one taken back made room for one more"
    );
    // Room the bound allows and the window has in no one run: leaves 0, 2, 4
    // and 15 free is 8 MiB, and no two of them touch.
    let leaf = REGION as u64;
    let window = lent.iter().map(|(_, mapping)| mapping.device_addr).min().expect("regions were lent");
    if lent.iter().any(|(_, mapping)| mapping.device_addr == window + 15 * leaf) {
        fail(format!("the window's last leaf, {:#x}, was lent though the bound had no room for it", window + 15 * leaf));
    }
    for n in [0, 2, 4] {
        let at = window + n * leaf;
        let index = lent
            .iter()
            .position(|(_, mapping)| mapping.device_addr == at)
            .unwrap_or_else(|| fail(format!("no region was lent at the window's leaf {n}, {at:#x}")));
        let (_, mapping) = lent.remove(index);
        dev.dma_unmap(mapping.device_addr).unwrap_or_else(|e| fail(format!("dma_unmap of leaf {n}: {e:?}")));
    }
    let wide = SharedMemory::create(2 * REGION).unwrap_or_else(|e| fail(format!("a region: {e:?}")));
    match dev.dma_map(wide.as_handle()) {
        Err(SyscallError::ResourceExhausted) => {}
        other => fail(format!(
            "a {}-byte region, with leaves 0, 2, 4 and 15 of the window free, was answered {other:?}",
            2 * REGION
        )),
    }
    println!(
        "blockd_io: with leaves 0, 2, 4 and 15 of the window free, a {}-byte region is refused with \
         ResourceExhausted",
        2 * REGION
    );
    println!("blockd_io: PASS dma-bound");
}

/// One region lent and taken back until ten times the domain's addresses went
/// by: none of it spends an address, and the device still reads into it.
fn dma_churn() {
    let (mut ctrl, region) = aim();
    let rounds = (10 * NARROW).div_ceil(REGION as u64);
    let mut addresses = std::collections::BTreeSet::new();
    for round in 0..rounds {
        let mapping = ctrl
            .claim()
            .dma_map(region.as_handle())
            .unwrap_or_else(|e| fail(format!("lend {round} of {rounds} was answered {e:?}")));
        addresses.insert(mapping.device_addr);
        ctrl.claim()
            .dma_unmap(mapping.device_addr)
            .unwrap_or_else(|e| fail(format!("taking lend {round} back was answered {e:?}")));
    }
    if addresses.len() as u64 > GRANT_TOTAL / REGION as u64 {
        fail(format!("{rounds} lends of one region were placed at {} device addresses", addresses.len()));
    }
    let mapping = ctrl.claim().dma_map(region.as_handle()).unwrap_or_else(|e| fail(format!("dma_map: {e:?}")));
    match transfer(&mut ctrl, 0, mapping.device_addr) {
        Ok(true) if is_block_zero(region.as_slice()) => {}
        other => fail(format!("a read into the region after the churn was answered {other:?}")),
    }
    println!(
        "blockd_io: {rounds} lends of a {REGION}-byte region, each taken back, {} MiB in all, at {} \
         device address(es); the device then read block 0 into it",
        rounds * REGION as u64 / (1024 * 1024),
        addresses.len()
    );
    println!("blockd_io: PASS dma-churn");
}

/// The controller's claim once the last holder's release has run, waited for with
/// no deadline: a process's end is published before the release its handles
/// queued has run (`issues/kernel/deferred-release-outlives-its-syscall.md`).
fn claim_when_free<T: toyos::endow::FromHandle>(syscap: &SysCap) -> T {
    loop {
        match syscap.claim_pci(BLOCKD) {
            // A pace, so the CPU this runs on can reach the idle loop that
            // drains the release.
            Err(SyscallError::AlreadyExists) => std::thread::sleep(std::time::Duration::from_millis(1)),
            Ok(claim) => return claim,
            Err(e) => fail(format!("the controller's claim was refused: {e:?}")),
        }
    }
}

/// On a boot where no release resets the function, the first claim's lent
/// address stays where the function may be aimed: the second claim lends
/// elsewhere and ends holding nothing, and the third still lends elsewhere.
fn dma_residue() {
    let syscap = capability();
    let first = {
        let dev: toyos::PciDev = claim_when_free(&syscap);
        let mut ctrl = Controller::open(dev, None).unwrap_or_else(|e| fail(format!("the controller: {e}")));
        let mut region = SharedMemory::create(REGION).unwrap_or_else(|e| fail(format!("a region: {e:?}")));
        region.as_mut_slice().fill(0xA5);
        let mapping = ctrl.claim().dma_map(region.as_handle()).unwrap_or_else(|e| fail(format!("dma_map: {e:?}")));
        match transfer(&mut ctrl, 0, mapping.device_addr) {
            Ok(true) if is_block_zero(region.as_slice()) => {}
            other => fail(format!("claim 1's read into its lent region was answered {other:?}")),
        }
        mapping.device_addr
    };
    println!("blockd_io: claim 1 lent a region at {first:#x}, the device read into it, and the claim ended holding it");
    for n in [2, 3] {
        let dev: toyos::PciDev = claim_when_free(&syscap);
        let region = SharedMemory::create(REGION).unwrap_or_else(|e| fail(format!("a region: {e:?}")));
        let mapping = dev.dma_map(region.as_handle()).unwrap_or_else(|e| fail(format!("claim {n}'s dma_map: {e:?}")));
        if mapping.device_addr == first {
            fail(format!("claim {n} lent a region at {first:#x}, where the unreset function was left aimed"));
        }
        dev.dma_unmap(mapping.device_addr).unwrap_or_else(|e| fail(format!("claim {n}'s dma_unmap: {e:?}")));
        println!("blockd_io: claim {n} lent at {:#x}, not {first:#x}, and ended holding nothing", mapping.device_addr);
    }
    println!("blockd_io: PASS dma-residue");
}

/// blockd started holding no controller answers a connection's first frame as
/// it answers every other: the malformed refused as such, a listing empty and
/// an open `NotFound`.
fn nothing() {
    let blockd = Blockd::with(None, &[]);
    let names = blockd.names();
    let region = || {
        let region = SharedMemory::create(REGION).unwrap_or_else(|e| fail(format!("a region: {e:?}")));
        vec![region.share().unwrap_or_else(|e| fail(format!("a second handle: {e:?}")))]
    };
    let absent = guid(ABSENT);
    let malformed = Some(Refusal::Malformed);
    for (what, msg_type, payload, handles, answered, refusal) in [
        ("a listing that carries a payload", wire::MSG_LIST, &[0u8; 4][..], vec![], wire::MSG_REFUSED, malformed),
        ("an open with no region", wire::MSG_OPEN, &absent[..], vec![], wire::MSG_REFUSED, malformed),
        ("an open whose GUID is short", wire::MSG_OPEN, &absent[..8], region(), wire::MSG_REFUSED, malformed),
        ("a listing", wire::MSG_LIST, &[][..], vec![], wire::MSG_LISTED, None),
        ("an open", wire::MSG_OPEN, &absent[..], region(), wire::MSG_REFUSED, Some(Refusal::NotFound)),
    ] {
        let conn = names.open(PORT).unwrap_or_else(|e| fail(format!("the port: {e:?}")));
        conn.send_bytes_with_handles(&handles, msg_type, payload).unwrap_or_else(|e| fail(format!("{what}: {e:?}")));
        let header = conn.recv_header().unwrap_or_else(|e| fail(format!("{what}'s answer: {e:?}")));
        let mut answer = [0u8; 64];
        let len = conn.recv_bytes(&header, &mut answer).unwrap_or_else(|e| fail(format!("{what}'s answer: {e:?}")));
        let got = Refusal::decode(&answer[..len]);
        if header.msg_type != answered || got != refusal || (refusal.is_none() && len != 0) {
            fail(format!("{what} was answered {} {got:?} in {len} bytes, not {answered} {refusal:?}", header.msg_type));
        }
        println!("blockd_io: with no controller, {what} was answered {answered} {refusal:?}");
    }
    drop(blockd);
    println!("blockd_io: PASS nothing");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("nothing") => nothing(),
        Some("claims") => claims(),
        Some("holder") => holder_role(args.get(2).map_or("", String::as_str)),
        Some("bench") => bench(),
        Some("hostile-head") => hostile_head(),
        Some("reset") => reset(),
        Some("crash") => crash(),
        Some(role @ ("dma-inside" | "dma-outside" | "dma-revoked" | "dma-after")) => dma(role),
        Some("dma-pool") => dma_pool(),
        Some("dma-bound") => dma_bound(),
        Some("dma-churn") => dma_churn(),
        Some("dma-residue") => dma_residue(),
        other => fail(format!("no role {other:?}")),
    }
}
