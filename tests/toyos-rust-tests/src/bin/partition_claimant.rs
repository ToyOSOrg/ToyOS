//! A GPT partition claimed as a device: its one holder reads and writes its
//! blocks and nobody else's, and nothing the kernel holds can be claimed.
//!
//! The disk is crafted and judged by `tests/common/partclaim.rs` on the host:
//! this binary's account of what it wrote is exactly what is in question, so
//! the verdict on the neighbours and on the target's bytes is read off the
//! image after the guest is gone. What this binary asserts is every refusal,
//! each with the word the ABI promises for it.

use std::io::Write;

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::PartitionDev;
use toyos_abi::part::{Block, PartGuid, PartitionName, BLOCK_BYTES, MAX_BLOCKS_PER_CALL};
use toyos_abi::syscall::{SyscallError, SYSCAP_LABEL};

/// Mirrored in `tests/common/partclaim.rs`: the partition this binary writes.
const TARGET_TYPE: &str = "7B1D4A3C-2E5F-4C8A-9D6B-0A1F2E3D4C5B";
/// Mirrored: the two FAT32 volumes either side of the target carry this type,
/// so a claim by it names two partitions.
const NEIGHBOUR_TYPE: &str = "5C3E8F21-9A4B-4D7E-8F10-2B3C4D5E6F70";
/// Mirrored: the partition `tests/partclaimcase/system.toml` grants test-runner.
const GRANTED_TYPE: &str = "A94F0E6D-3B2C-4E1A-8C7D-6E5F4A3B2C1D";
/// Mirrored: the target's length in partition blocks.
const TARGET_BLOCKS: u64 = 2048;
/// Mirrored: the `/home` file written between the target's transfers, so the
/// kernel's own writes to the same disk are interleaved with the claim's.
const HOME_FILE: &str = "/home/partclaim-interleaved.bin";
const HOME_CHUNK: usize = 32 * 1024;

/// The types of the partitions the kernel mounts on this boot: the ESP
/// (`/boot`), the log partition (Microsoft Basic Data, `/log`), ROOT
/// (`/system`) and DATA (`/home`). One of each on the machine.
const MOUNTED: [(&str, &str); 4] = [
    ("the ESP", "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"),
    ("the log partition", "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"),
    ("ROOT", "B350BC93-BB6A-4C5E-9589-A5C3CFD555FD"),
    ("DATA", "064E3777-5076-4C71-8E07-90AD24CFE8D6"),
];

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

fn guid(text: &str) -> PartGuid {
    PartGuid::parse(text).unwrap_or_else(|| panic!("{text} is not a GUID"))
}

fn claim(cap: &SysCap, name: PartitionName) -> Result<PartitionDev, SyscallError> {
    cap.claim_partition::<PartitionDev>(name)
}

fn refused(cap: &SysCap, what: &str, name: PartitionName, want: SyscallError) {
    match claim(cap, name) {
        Err(got) if got == want => println!("partition_claimant: {what} refused with {got:?}"),
        Err(got) => panic!("{what}: expected {want:?}, got {got:?}"),
        Ok(_) => panic!("{what}: expected {want:?}, and the claim was minted"),
    }
}

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");

    for (what, ty) in MOUNTED {
        refused(
            &cap,
            &format!("{what}, which the kernel has mounted,"),
            PartitionName::OfType(guid(ty)),
            SyscallError::PermissionDenied,
        );
    }
    refused(
        &cap,
        "the partition init granted test-runner",
        PartitionName::OfType(guid(GRANTED_TYPE)),
        SyscallError::AlreadyExists,
    );
    refused(
        &cap,
        "a type two partitions carry",
        PartitionName::OfType(guid(NEIGHBOUR_TYPE)),
        SyscallError::InvalidArgument,
    );
    refused(
        &cap,
        "a type no partition carries",
        PartitionName::OfType(guid("00000000-0000-0000-0000-000000000001")),
        SyscallError::NotFound,
    );
    refused(
        &cap,
        "the all-zero GUID, which GPT means as an unused entry,",
        PartitionName::OfType(PartGuid([0; 16])),
        SyscallError::NotFound,
    );

    let target = claim(&cap, PartitionName::OfType(guid(TARGET_TYPE)))
        .expect("the target partition is claimable");
    let info = target.describe().expect("a partition claim describes itself");
    assert_eq!(info.blocks, TARGET_BLOCKS, "the claim's length is the partition's");
    assert_eq!(info.of_type(), guid(TARGET_TYPE), "the claim is of the type it was named by");
    println!("partition_claimant: claimed {} blocks", info.blocks);

    // One partition has one holder, whichever of its names a second claim uses.
    refused(
        &cap,
        "the target, a second time by type,",
        PartitionName::OfType(guid(TARGET_TYPE)),
        SyscallError::AlreadyExists,
    );
    refused(
        &cap,
        "the target, a second time by its unique GUID,",
        PartitionName::Unique(info.unique()),
        SyscallError::AlreadyExists,
    );

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

    // Letting the last handle go is what releases the partition. The release
    // is deferred to a drain another CPU may be running when `close` returns
    // (issues/kernel/deferred-release-outlives-its-syscall.md), so the claim
    // is asked again for a bounded second rather than once.
    drop(target);
    let again = (0..1000)
        .find_map(|_| match claim(&cap, PartitionName::Unique(info.unique())) {
            Err(SyscallError::AlreadyExists) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
                None
            }
            other => Some(other),
        })
        .expect("the target was still held a second after its holder let it go")
        .expect("the target is claimable again once its holder let it go");
    drop(again);

    println!("partition_claimant: PASS");
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
