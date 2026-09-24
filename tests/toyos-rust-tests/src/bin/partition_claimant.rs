//! A GPT partition claimed as a device: its one holder reads and writes its
//! blocks and nobody else's, nothing the kernel holds can be claimed, and a
//! claim's fsync answers for its own writes.
//!
//! The disks are crafted and judged by `tests/common/partclaim.rs` on the
//! host: this binary's account of what it wrote is exactly what is in
//! question, so the verdict on the neighbours and on the target's bytes is read
//! off the images after the guest is gone. What this binary asserts is every
//! refusal, each with the word the ABI promises for it.
//!
//! Roles, by the first argument:
//! - `main <ESP> <LOG> <ROOT>` — every refusal, the idle ROOT slot written
//!   whole and read back, and both releases; the three GUIDs are the boot
//!   stick's, which the host drew and this binary cannot know;
//! - `holder` — claims the target, says so, and waits to be killed;
//! - `endowed` — finds the claim its parent moved to it, by the label init
//!   endows a `part:` row under;
//! - `unanswered` — a claim while a disk does not answer a read of its table;
//! - `deadman` — transfers whose every attempt is refused on its budget until
//!   the deadman;
//! - `departure` — two claims on one USB stick whose device leaves owing a
//!   flush and comes back, and whose fsyncs answer for their own writes.

use std::io::{BufRead, BufReader, Write};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::{AsHandle, PartitionDev};
use toyos_abi::part::{Block, PartGuid, BLOCK_BYTES, MAX_BLOCKS_PER_CALL};
use toyos_abi::syscall::{DeviceType, SyscallError, DEV_PREFIX, SYSCAP_LABEL, SYS_DEVICE_CLAIM};

const SELF_PATH: &str = "/system/bin/test_rs_partition_claimant";

/// Mirrored in `tests/common/partclaim.rs`: the idle ROOT slot this binary
/// writes whole — a TOYOS-ROOT partition the boot probed and did not mount.
const TARGET: &str = "7B1D4A3C-2E5F-4C8A-9D6B-0A1F2E3D4C5B";
/// Mirrored, and in `tests/partclaimcase/system.toml`: the partition init
/// grants test-runner.
const GRANTED: &str = "A94F0E6D-3B2C-4E1A-8C7D-6E5F4A3B2C1D";
/// Mirrored: a partition whose length is not whole 4 KiB blocks.
const MISALIGNED: &str = "3E8A1C5F-7D2B-4F60-9A1E-5C4B3D2E1F07";
/// Mirrored: a unique GUID the NVMe disk and the USB stick both carry.
const TWIN: &str = "6D2F9B41-8C3E-4A57-B1D0-2E4F6A8C0B13";
/// Mirrored: DATA, which the kernel mounts at `/home`.
const DATA: &str = "E3A7C5D9-1B2F-4E6A-8D0C-9F7B5A3E1C24";
/// Mirrored: the two partitions of the stick whose device leaves.
const DEPARTING: &str = "1F3E5D7C-9B2A-4C6E-8F01-A3B5C7D9E2F4";
const STAYING: &str = "2A4C6E80-1B3D-4F57-9E6A-C8D0B2F4A6E1";

/// Mirrored: the target's length in blocks.
const TARGET_BLOCKS: u64 = 2048;
/// Mirrored: the `/home` file written between the target's transfers, so the
/// kernel's own writes to the same disk are interleaved with the claim's.
const HOME_FILE: &str = "/home/partclaim-interleaved.bin";
const HOME_CHUNK: usize = 32 * 1024;

/// Mirrored: what block `n` of the target holds once this binary is done.
fn pattern(n: u64) -> Block {
    let mut block = [0u8; BLOCK_BYTES];
    for (i, byte) in block.iter_mut().enumerate() {
        *byte = (n as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    block[..8].copy_from_slice(&n.to_le_bytes());
    block[8..24].copy_from_slice(b"TOYOS-PARTCLAIM\0");
    block
}

/// Mirrored: what the refused writes past the end carry, so a block of the
/// neighbour holding it says which write reached it.
const PAST_END: &[u8; 16] = b"TOYOS-PAST-END\0\0";

/// Mirrored: what the departure writes, block `n` of partition `which`.
fn departure_block(which: u8, n: u64) -> Block {
    let mut block = [which; BLOCK_BYTES];
    block[..8].copy_from_slice(&n.to_le_bytes());
    block[8..24].copy_from_slice(b"TOYOS-DEPARTURE\0");
    block
}

fn guid(text: &str) -> PartGuid {
    PartGuid::parse(text).unwrap_or_else(|| panic!("{text} is not a GUID"))
}

fn claim(cap: &SysCap, name: PartGuid) -> Result<PartitionDev, SyscallError> {
    cap.claim_partition::<PartitionDev>(name)
}

fn said(what: &str, got: SyscallError) {
    println!("partition_claimant: {what} refused with {got:?}");
}

fn refused(cap: &SysCap, what: &str, name: PartGuid, want: SyscallError) {
    match claim(cap, name) {
        Err(got) if got == want => said(what, got),
        Err(got) => panic!("{what}: expected {want:?}, got {got:?}"),
        Ok(_) => panic!("{what}: expected {want:?}, and the claim was minted"),
    }
}

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("main") => test(&cap, &args[1..]),
        Some("holder") => holder(&cap),
        Some("endowed") => endowed(),
        Some("unanswered") => unanswered(&cap),
        Some("deadman") => deadman(&cap),
        Some("departure") => departure(&cap),
        other => panic!("unknown role {other:?}"),
    }
}

fn test(cap: &SysCap, boot_stick: &[String]) {
    let [esp, log, root] = boot_stick else {
        panic!("main takes the boot stick's ESP, log and ROOT GUIDs, got {boot_stick:?}");
    };
    for (what, name) in [("the ESP", esp.as_str()), ("the log partition", log), ("ROOT", root), ("DATA", DATA)]
    {
        refused(
            cap,
            &format!("{what}, which the kernel has mounted,"),
            guid(name),
            SyscallError::PermissionDenied,
        );
    }

    // init minted this one for test-runner from the manifest's `part:` row.
    refused(cap, "the partition init granted test-runner", guid(GRANTED), SyscallError::AlreadyExists);

    refused(
        cap,
        "a unique GUID two disks carry",
        guid(TWIN),
        SyscallError::InvalidArgument,
    );
    refused(
        cap,
        "a partition that is not whole 4 KiB blocks",
        guid(MISALIGNED),
        SyscallError::NotSupported,
    );
    refused(
        cap,
        "a GUID no partition carries",
        guid("00000000-0000-0000-0000-000000000001"),
        SyscallError::NotFound,
    );
    refused(
        cap,
        "the all-zero GUID, which GPT means as an unused entry,",
        PartGuid([0; 16]),
        SyscallError::NotFound,
    );
    unread_selector_words(cap);

    let target = claim(cap, guid(TARGET)).expect("the idle ROOT slot is claimable");
    let info = target.describe().expect("a partition claim describes itself");
    assert_eq!(info.blocks, TARGET_BLOCKS, "the claim's length is the partition's");
    assert_eq!(info.unique(), guid(TARGET), "the claim is the partition it was named by");
    println!("partition_claimant: claimed {} blocks", info.blocks);
    refused(cap, "the target, a second time,", guid(TARGET), SyscallError::AlreadyExists);

    past_the_end(&target, info.blocks);

    // The whole partition, first block to last, with the kernel's own writes
    // to `/home` — the same NVMe disk — between the runs.
    let mut home = std::fs::File::create(HOME_FILE).expect("create the /home file");
    let runs = info.blocks.div_ceil(MAX_BLOCKS_PER_CALL as u64);
    for run in 0..runs {
        let first = run * MAX_BLOCKS_PER_CALL as u64;
        let count = (info.blocks - first).min(MAX_BLOCKS_PER_CALL as u64);
        let blocks: Vec<Block> = (first..first + count).map(pattern).collect();
        target.write(first, &blocks).unwrap_or_else(|e| panic!("write at {first}: {e:?}"));
        if run % 8 == 0 {
            let piece: Vec<u8> = (0..HOME_CHUNK).map(|i| (run as usize ^ i) as u8).collect();
            home.write_all(&piece).expect("append to the /home file");
            home.sync_all().expect("the /home file is durable");
        }
    }
    drop(home);
    target.sync().expect("the claim's writes are durable");

    let mut back = vec![[0u8; BLOCK_BYTES]; MAX_BLOCKS_PER_CALL];
    for run in 0..runs {
        let first = run * MAX_BLOCKS_PER_CALL as u64;
        let count = (info.blocks - first).min(MAX_BLOCKS_PER_CALL as u64) as usize;
        target.read(first, &mut back[..count]).unwrap_or_else(|e| panic!("read at {first}: {e:?}"));
        for (i, block) in back[..count].iter().enumerate() {
            let n = first + i as u64;
            assert!(*block == pattern(n), "block {n} did not read back as written");
        }
    }
    println!("partition_claimant: wrote and read back {} blocks", info.blocks);

    // Letting the last handle go is what releases the partition.
    drop(target);
    drop(reclaimed(cap, "its holder closed it"));

    // A holder that dies never closes anything: teardown is what gives the
    // partition back.
    let mut holder = child(cap, "holder").stdout(Stdio::piped()).spawn().expect("spawn holder");
    let mut out = BufReader::new(holder.stdout.take().expect("holder stdout"));
    let mut line = String::new();
    out.read_line(&mut line).expect("the holder's ready line");
    assert_eq!(line.trim(), "held", "the holder did not claim the target: {line:?}");
    refused(cap, "the target, while another process holds it,", guid(TARGET), SyscallError::AlreadyExists);
    holder.kill().expect("kill the holder");
    holder.wait().expect("reap the holder");
    drop(reclaimed(cap, "its holder was killed"));

    // A claim moved to a child under the label init writes for a `part:` row —
    // `dev:` and the row's own spelling — is the one `endow::partition` finds.
    let moved = cap
        .claim_partition::<toyos::Device>(guid(TARGET))
        .expect("the target is claimable to move");
    let status = child(cap, "endowed")
        .endow(&format!("{DEV_PREFIX}part:{TARGET}"), moved.into_raw().0)
        .status()
        .expect("run the endowed child");
    assert!(status.success(), "the endowed child did not find its claim: {status:?}");
    drop(reclaimed(cap, "the child it was moved to exited"));

    println!("partition_claimant: PASS");
}

/// The target claimed again once `how`. The release is deferred to a drain
/// another CPU may be running when `close` or the kill returns
/// (issues/kernel/deferred-release-outlives-its-syscall.md), so the claim is
/// asked again for a bounded second rather than once.
fn reclaimed(cap: &SysCap, how: &str) -> PartitionDev {
    (0..1000)
        .find_map(|_| match claim(cap, guid(TARGET)) {
            Err(SyscallError::AlreadyExists) => {
                std::thread::sleep(Duration::from_millis(1));
                None
            }
            other => Some(other),
        })
        .unwrap_or_else(|| panic!("the target was still held a second after {how}"))
        .unwrap_or_else(|e| panic!("the target is not claimable again once {how}: {e:?}"))
}

/// This binary's children mint their own claims, so each is endowed a
/// duplicate of the capability.
fn child(cap: &SysCap, role: &str) -> Command {
    let mut command = Command::new(SELF_PATH);
    let dup = cap.duplicate().expect("duplicate the capability for a child");
    command.arg(role).endow(SYSCAP_LABEL, dup.into_raw().0);
    command
}

fn holder(cap: &SysCap) {
    let target = claim(cap, guid(TARGET)).expect("the holder claims the target");
    println!("held");
    std::io::stdout().flush().expect("flush the ready line");
    loop {
        std::thread::sleep(Duration::from_secs(60));
        let _ = &target;
    }
}

/// The claim the parent moved here, found by `endow::partition` under the
/// label a `part:` row is endowed with.
fn endowed() {
    let target: PartitionDev =
        toyos::endow::partition(guid(TARGET)).expect("the moved claim is in the endowment table");
    let info = target.describe().expect("the moved claim describes itself");
    assert_eq!(info.unique(), guid(TARGET), "the endowment is the partition its label names");
    let mut first = [[0u8; BLOCK_BYTES]];
    target.read(0, &mut first).expect("read through the moved claim");
    assert!(first[0] == pattern(0), "the moved claim reads what the parent wrote");
}

/// `SYS_DEVICE_CLAIM` with every selector word given, which no typed wrapper
/// can spell: a word the class does not read is refused, never dropped.
fn unread_selector_words(cap: &SysCap) {
    fn raw(a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
        let ret: u64;
        // SAFETY: a register-only `syscall`; no argument is a pointer.
        unsafe {
            core::arch::asm!(
                "syscall",
                in("rdi") SYS_DEVICE_CLAIM,
                in("rsi") a1,
                in("rdx") a2,
                in("r8") a3,
                in("r9") a4,
                lateout("rax") ret,
                out("rcx") _,
                out("r11") _,
            );
        }
        ret
    }
    let handle = cap.as_handle().0 as u64;
    for (what, class, words) in [
        ("a mouse claim carrying a first selector word", DeviceType::Mouse, [1, 0]),
        ("a mouse claim carrying a second selector word", DeviceType::Mouse, [0, 1]),
        ("a PCI claim carrying a second selector word", DeviceType::PciFunction, [0x1af4_1041, 1]),
    ] {
        match SyscallError::from_u64(raw(handle, class as u64, words[0], words[1])) {
            Some(SyscallError::InvalidArgument) => said(what, SyscallError::InvalidArgument),
            other => panic!("{what}: expected InvalidArgument, got {other:?}"),
        }
    }
}

/// Every transfer that does not end inside the partition is refused before it
/// reaches the device, reads included: a read past the end is somebody else's
/// data.
fn past_the_end(target: &PartitionDev, blocks: u64) {
    let mut marked = [0u8; BLOCK_BYTES];
    marked[..PAST_END.len()].copy_from_slice(PAST_END);
    let one = [marked];
    let two = [marked, marked];
    let long = vec![marked; MAX_BLOCKS_PER_CALL + 1];

    let cases: [(&str, Result<(), SyscallError>); 5] = [
        ("one block at the partition's length", target.write(blocks, &one)),
        ("two blocks from its last", target.write(blocks - 1, &two)),
        ("a first block that overflows", target.write(u64::MAX, &one)),
        ("more blocks than one call carries", target.write(0, &long)),
        ("no blocks at all", target.write(0, &[])),
    ];
    for (what, got) in cases {
        assert_eq!(got, Err(SyscallError::InvalidArgument), "a write of {what}");
    }
    let mut into = [[0u8; BLOCK_BYTES]];
    assert_eq!(
        target.read(blocks, &mut into),
        Err(SyscallError::InvalidArgument),
        "a read of one block at the partition's length"
    );
    println!("partition_claimant: every transfer past the end refused");
}

/// A disk that did not answer a read of its table makes the answer unknown:
/// the claim is refused, not resolved on the disks that did answer.
fn unanswered(cap: &SysCap) {
    refused(
        cap,
        "the target, while its disk does not answer a read of its table,",
        guid(TARGET),
        SyscallError::NotSupported,
    );
    println!("partition_claimant: PASS");
}

/// Every attempt of a transfer is refused on its budget until the deadman: each
/// ends `Io`, the device's word, and not another ask-again. (NVMe's flush asks
/// the device nothing, so it has no budget to refuse.)
fn deadman(cap: &SysCap) {
    let target = claim(cap, guid(TARGET)).expect("the target is claimable");
    let mut one = [[0u8; BLOCK_BYTES]];
    assert_eq!(target.write(0, &one), Err(SyscallError::Io), "a write past the deadman");
    assert_eq!(target.read(0, &mut one), Err(SyscallError::Io), "a read past the deadman");
    println!("partition_claimant: PASS");
}

/// Two claims on one USB stick. `STAYING` writes and is flushed; `DEPARTING`
/// writes a block, and its next write is the one `usb-transport-break-owed`
/// breaks, so the stick's device leaves holding that first block unflushed and
/// the host moves it to another port. When it is back, the first flush asked
/// is `STAYING`'s, which wrote nothing that was lost, and the second is
/// `DEPARTING`'s, which did: each answers for its own writes, whichever comes
/// first.
fn departure(cap: &SysCap) {
    let staying = claim(cap, guid(STAYING)).expect("the staying partition is claimable");
    let departing = claim(cap, guid(DEPARTING)).expect("the departing partition is claimable");

    staying.write(0, &[departure_block(b'S', 0)]).expect("the staying write");
    staying.sync().expect("the staying write, flushed before anything left");
    departing.write(0, &[departure_block(b'D', 0)]).expect("the first departing write");
    println!("partition_claimant: a write is reported and not flushed");
    departing
        .write(1, &[departure_block(b'D', 1)])
        .expect("the write the device left under, sent again on it when it came back");
    println!("partition_claimant: the write the device left under completed");

    assert_eq!(
        staying.sync(),
        Ok(()),
        "a claim that wrote nothing the departure lost was told of the loss"
    );
    assert_eq!(
        departing.sync(),
        Err(SyscallError::Io),
        "a claim whose write was in the cache of the device that left was told it is durable"
    );
    said("the departing claim's flush, over a write the device lost,", SyscallError::Io);
    assert_eq!(departing.sync(), Ok(()), "a loss is told once");

    departing
        .write(0, &[departure_block(b'D', 0), departure_block(b'D', 1)])
        .expect("the lost write, written again");
    departing.sync().expect("the rewrite is durable");
    let mut back = [[0u8; BLOCK_BYTES]; 2];
    departing.read(0, &mut back).expect("read the rewrite back");
    assert!(back[0] == departure_block(b'D', 0) && back[1] == departure_block(b'D', 1));
    println!("partition_claimant: PASS");
}
