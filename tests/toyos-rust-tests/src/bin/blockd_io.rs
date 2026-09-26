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
//! - `bench` — the same bytes through the kernel's driver (a partition claim on
//!   the first controller) and through blockd, timed;
//! - `reset` — blockd started withholding its second answer: the silence ends
//!   in a controller reset, the withheld write is answered not done, and the
//!   write acknowledged before it is on the medium after the next flush;
//! - `crash` — a FAT32 volume written through a session, blockd killed with a
//!   write on the wire, restarted, and the same volume carried on;
//! - `dma-inside`, `dma-outside`, `dma-revoked`, `dma-after` — the controller
//!   aimed by this process at a lent region, past it, and at one taken back.

use std::io::{BufRead, BufReader, Write};
use std::os::toyos::process::{ChildExt, CommandExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use blockd::nvme::Controller;
use blockd::{Error, Outcome, Session, Unsent};
use toyos::endow::Endowments;
use toyos::namespace::{self, Namespace};
use toyos::port::{self, Acceptor, Connector};
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::AsHandle;
use toyos_abi::part::PartGuid;
use toyos_abi::syscall::{PciId, SyscallError, DEV_PREFIX, SERVE_PREFIX, SYSCAP_LABEL};
use toyos_blockring::wire::Refusal;
use toyos_blockring::{BLOCK_BYTES, MAX_REQUEST_BLOCKS, PORT};

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
/// Mirrored: the kernel's disk, on the first controller.
const KBENCH: &str = "D4F6B8CA-3E5A-4C79-9D2F-A4B6C8EA02C3";
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

/// How long a restart waits for the claim of the process it stopped to come
/// back. **A compromise over a kernel defect, the one init makes for the same
/// reason** (`issues/kernel/deferred-release-outlives-its-syscall.md`): a
/// process's end is published before the release its handles queued has run.
const CLAIM_RETURN: Duration = Duration::from_secs(2);

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

/// blockd, held by this process: its claim minted here, the port's acceptor
/// kept here, so a blockd can end and another take its place on the same
/// name. What blockd says goes to this process's stdout, a line at a time.
struct Blockd {
    syscap: SysCap,
    acceptor: Acceptor,
    connector: Connector,
    child: Option<Child>,
}

impl Blockd {
    fn start(args: &[&str]) -> Self {
        Self::with(capability(), args)
    }

    fn with(syscap: SysCap, args: &[&str]) -> Self {
        let (acceptor, connector) = port::create().unwrap_or_else(|e| fail(format!("no port: {e:?}")));
        let mut blockd = Self { syscap, acceptor, connector, child: None };
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
        let asked = Instant::now();
        let claim: toyos::Device = loop {
            match self.syscap.claim_pci(BLOCKD) {
                Err(SyscallError::AlreadyExists) if asked.elapsed() < CLAIM_RETURN => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(claim) => break claim,
                Err(e) => fail(format!("the controller's claim was refused: {e:?}")),
            }
        };
        let acceptor = toyos_abi::syscall::dup(self.acceptor.as_handle())
            .unwrap_or_else(|e| fail(format!("the acceptor would not duplicate: {e:?}")));
        let mut command = Command::new("/system/bin/blockd");
        command.args(args);
        command.stdout(Stdio::piped());
        command.endow(&format!("{DEV_PREFIX}pci:8086:5845"), claim.into_raw().0);
        command.endow(&format!("{SERVE_PREFIX}{PORT}"), acceptor.0);
        let mut child = command.spawn().unwrap_or_else(|e| fail(format!("blockd did not start: {e}")));
        let out = child.stdout.take().expect("piped");
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
            }
        });
        self.child = Some(child);
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
/// how long the flushes took and how many there were: an acknowledged write
/// holds its arena blocks until a flush covers it, so a full arena is where
/// one is asked.
fn write_all(s: &mut Session, chunks: &[Vec<u8>], in_flight: usize) -> (Duration, u32) {
    let mut next = 0usize;
    let mut lba = 0u64;
    let mut outstanding = 0usize;
    let mut full = false;
    let (mut flushing, mut flushes) = (Duration::ZERO, 0u32);
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
            let started = Instant::now();
            flushed(s);
            flushing += started.elapsed();
            flushes += 1;
            full = false;
        }
    }
    let started = Instant::now();
    flushed(s);
    (flushing + started.elapsed(), flushes + 1)
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

fn mb_per_s(blocks: u64, took: Duration) -> f64 {
    (blocks * BLOCK_BYTES as u64) as f64 / (1024.0 * 1024.0) / took.as_secs_f64()
}

/// The same bytes through the kernel's driver and through blockd, the data
/// built before and checked after what is timed, so each number is the
/// driver's path and nothing of this binary's.
fn bench() {
    // The kernel's driver, through a partition claim on the first controller:
    // one request at a time, as it moves them.
    let syscap = capability();
    let part: toyos::PartitionDev = syscap
        .claim_partition(PartGuid(guid(KBENCH)))
        .unwrap_or_else(|e| fail(format!("the kernel's bench partition was refused: {e:?}")));
    let written = chunks(BENCH_BLOCKS, 0x3C);
    let blocks: Vec<Vec<[u8; BLOCK_BYTES]>> = written
        .iter()
        .map(|c| c.chunks(BLOCK_BYTES).map(|b| b.try_into().expect("a block")).collect())
        .collect();
    let started = Instant::now();
    let mut lba = 0u64;
    for chunk in &blocks {
        part.write(lba, chunk).unwrap_or_else(|e| fail(format!("a kernel write: {e:?}")));
        lba += chunk.len() as u64;
    }
    part.sync().unwrap_or_else(|e| fail(format!("the kernel's fsync: {e:?}")));
    let kernel_write = started.elapsed();
    let mut read = vec![[0u8; BLOCK_BYTES]; BENCH_BLOCKS as usize];
    let per = toyos_abi::part::MAX_BLOCKS_PER_CALL;
    let started = Instant::now();
    for (i, chunk) in read.chunks_mut(per).enumerate() {
        part.read((i * per) as u64, chunk).unwrap_or_else(|e| fail(format!("a kernel read: {e:?}")));
    }
    let kernel_read = started.elapsed();
    holds(&read.concat(), &written, "the kernel's bench partition");
    drop(part);

    // blockd, one request at a time and then as many as the arena holds.
    let blockd = Blockd::with(syscap, &[]);
    let mut s = open(blockd.names(), BENCH);
    let mut runs = Vec::new();
    for (salt, in_flight) in [(0x3D, 1usize), (0x3C, 15)] {
        let written = chunks(BENCH_BLOCKS, salt);
        let started = Instant::now();
        let (flushing, flushes) = write_all(&mut s, &written, in_flight);
        let write = started.elapsed();
        let started = Instant::now();
        let read = read_all(&mut s, BENCH_BLOCKS, in_flight);
        let took = started.elapsed();
        holds(&read, &written, "blockd's bench partition");
        runs.push(format!(
            "{in_flight} in flight: write {:.1} MiB/s ({flushes} Flushes, {} ms of it) read {:.1} MiB/s",
            mb_per_s(BENCH_BLOCKS, write),
            flushing.as_millis(),
            mb_per_s(BENCH_BLOCKS, took),
        ));
    }
    println!(
        "blockd_io: bench {} MiB each way: kernel driver write {:.1} MiB/s (its one fsync issues no \
         Flush) read {:.1} MiB/s; blockd {}; at most {} requests on the wire",
        BENCH_BLOCKS * BLOCK_BYTES as u64 / (1024 * 1024),
        mb_per_s(BENCH_BLOCKS, kernel_write),
        mb_per_s(BENCH_BLOCKS, kernel_read),
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
    let started = Instant::now();
    match s.write(1, &pattern(0x71, 1)) {
        Ok(Outcome::Device) => {}
        other => fail(format!("the withheld write was answered {other:?}, not Device")),
    }
    println!(
        "blockd_io: the withheld write was answered Device after {} ms",
        started.elapsed().as_millis()
    );
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
            match ctrl.transfer(false, 0, 1, mapping.device_addr) {
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
    let answered = ctrl.transfer(false, 0, 1, at);
    println!("blockd_io: the device answered a read aimed {what} with {answered:?}");
    if !ctrl.refused_within(Duration::from_secs(5)) {
        fail(format!("a read aimed {what} left the claim answering; the unit did not refuse it"));
    }
    if !region.as_slice().iter().all(|b| *b == 0xA5) {
        fail(format!("a read aimed {what} changed the lent region"));
    }
    println!(
        "blockd_io: the unit refused a read aimed {what}, the claim answers the refusal, and the \
         region is untouched"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("claims") => claims(),
        Some("holder") => holder_role(args.get(2).map_or("", String::as_str)),
        Some("bench") => bench(),
        Some("reset") => reset(),
        Some("crash") => crash(),
        Some(role @ ("dma-inside" | "dma-outside" | "dma-revoked" | "dma-after")) => dma(role),
        other => fail(format!("no role {other:?}")),
    }
}
