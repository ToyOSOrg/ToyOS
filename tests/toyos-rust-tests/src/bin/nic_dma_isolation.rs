//! What a NIC claim grants is buffers, and the device's virtqueue is not in it.
//!
//! **The holder of `DeviceType::Nic` writes every byte it is granted.** Every
//! shared mapping is writable — `SharedRegion::map_into` passes `writable =
//! true` and has no other form — so whatever is in that page is the claimant's
//! to rewrite whenever it likes. Until 2026-08-23 the page was the driver's
//! whole `DmaPool`: the RX descriptor table sat at offset 0, the RX available
//! and used rings behind it and the entire TX virtqueue at `0x3000`, and only
//! from `0x4000` on was it the frames `netd` was meant to have. Each of the 256
//! RX descriptors carries the physical address the NIC will DMA the next frame
//! into and all 256 are posted at bind, so at rest the claimant held 2 KiB of
//! live DMA targets the device had not read yet — rewriting one aimed the NIC
//! at kernel text, a page table or another process, and rewriting the TX
//! descriptor read arbitrary physical memory back out onto the wire. Nothing
//! else stood in the way: `kernel/src/iommu/mod.rs` says of itself that it
//! "refuses nothing".
//!
//! This binary is that claimant. It holds the claim rather than talking to
//! `netd`, because the subject is what the *kernel* hands over and netd is only
//! the program that happens to receive it — `tests/testcases` runs no netd, and
//! the estate's `SysCap` carries `Rights::DEVICE`, so the claim is this
//! process's to take.
//!
//! Three arms, and the order is load-bearing.
//!
//! 1. **Transmit once**, so the driver has written a TX descriptor somewhere.
//!    Its `len` field is [`FRAME_LEN`], a number nothing else in the page is.
//! 2. **Look for either virtqueue in the granted page** — a run of
//!    `rx_buf_count` descriptors whose `len` is the RX buffer size and whose
//!    flags say the device writes them, and the transmit's own descriptor. Both
//!    are read out of the page the way the device reads them, at the layout
//!    virtio 1.2 §2.7.5 gives a descriptor. On the driver this test was written
//!    against, the first is at offset 0 and the second at `0x3000`.
//! 3. **Scribble every byte of the page and keep driving the device.** A
//!    transmit that returns is a used ring the claimant did not just rewrite:
//!    `Virtqueue::submit_and_wait` spins on it, so a driver whose TX rings were
//!    in this page would never come back from the first one. Then look for both
//!    descriptors again, because a driver that moved only the RX queue would
//!    write the TX one into the poison.
//!
//! Arm 2 is the deterministic half and it runs before anything is written, so a
//! kernel with the old layout fails by name in milliseconds rather than wedging
//! the boot.

use std::process::exit;

use toyos::endow::Endowments;
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::Nic;
use toyos_abi::syscall::{DeviceType, SYSCAP_LABEL};

/// The page a claim maps: one 2 MiB frame, which is what `netd` adopts it as
/// (`userland/netd/src/main.rs`) and what the kernel's `Region` declares.
const REGION_BYTES: usize = 2 * 1024 * 1024;

/// One split-virtqueue descriptor: `addr` (le64), `len` (le32), `flags` (le16),
/// `next` (le16) — virtio 1.2 §2.7.5.
const DESC_BYTES: usize = 16;
/// §2.7.5.3, `VIRTQ_DESC_F_WRITE`: the device writes into this buffer. Every RX
/// descriptor this driver posts carries it and no TX descriptor does.
const DESC_F_WRITE: u16 = 2;

/// The frame this test submits, and a fingerprint as much as a length: nothing
/// else in the page is this number, so a descriptor whose `len` is this is one
/// that this transmit wrote.
const FRAME_LEN: usize = 1499;
/// What the frame is made of, past its net header. Chosen so no sixteen aligned
/// bytes of it can be read as a descriptor of [`FRAME_LEN`] bytes.
const FRAME_BYTE: u8 = 0xEE;
/// What the claimant writes over every byte it was granted.
const POISON: u8 = 0xA5;
/// Transmits after the poison. One proves the used ring survived; a handful
/// says the queue kept working rather than answered once.
const TRANSMITS: usize = 8;

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let nic: Nic = cap
        .claim(DeviceType::Nic)
        .expect("this boot's profile carries a virtio-net and no program on it claims one");
    // Once: the kernel installs the description's handles on the first read and
    // answers a second with the same numbers.
    let info = nic.info().expect("a NIC claim answers its own description");

    let rx_count = info.rx_buf_count as usize;
    let rx_size = info.rx_buf_size as usize;
    let rx_off = info.rx_buf_offset as usize;
    let tx_off = info.tx_buf_offset as usize;
    let hdr = info.net_hdr_size as usize;
    // The description has to fit in the page it describes; a kernel that says
    // otherwise is not something to go on and read.
    assert!(
        rx_off + rx_count * rx_size <= REGION_BYTES && tx_off + FRAME_LEN <= REGION_BYTES,
        "the claim describes buffers past its own 2 MiB page: rx {rx_off:#x}+{rx_count}x{rx_size}, \
         tx {tx_off:#x}"
    );
    assert!(hdr < FRAME_LEN, "a net header of {hdr} bytes leaves no frame in {FRAME_LEN}");

    let mut region =
        SharedMemory::adopt(info.dma, REGION_BYTES).expect("map the page the claim granted");

    // Arm 1: one transmit, so a TX descriptor exists to be looked for.
    transmit(&nic, &mut region, tx_off, hdr);

    // Arm 2: is either virtqueue in the page? Before a byte of it is written,
    // so the old layout fails here rather than in the poison below.
    if let Some(off) = find_rx_table(region.as_slice(), rx_count, rx_size) {
        eprintln!(
            "the NIC claim granted a page whose offset {off:#x} is this device's RX descriptor \
             table: {rx_count} live DMA targets the claimant can rewrite"
        );
        exit(1);
    }
    if let Some(off) = find_tx_desc(region.as_slice()) {
        eprintln!(
            "the NIC claim granted a page whose offset {off:#x} is the descriptor this transmit \
             wrote: the claimant can point the device at any physical address it likes"
        );
        exit(1);
    }

    // Arm 3: rewrite everything the claim granted, then keep using the device.
    region.as_mut_slice().fill(POISON);
    let mut received = 0usize;
    for _ in 0..TRANSMITS {
        // A transmit that never returns is the failure this arm is for:
        // `submit_and_wait` spins on the used ring, so a ring inside the poison
        // above would hang the kernel here and the harness would time this test
        // out rather than fail it.
        transmit(&nic, &mut region, tx_off, hdr);
        let polled = nic.rx_poll().expect("a NIC claim answers its own poll");
        if polled != 0 {
            let buf_idx = polled >> 16;
            nic.rx_done(buf_idx).expect("give the buffer back");
            received += 1;
        }
    }

    // A driver that moved only the RX queue would have written the TX
    // descriptor into the poison during the transmits above.
    if let Some(off) = find_rx_table(region.as_slice(), rx_count, rx_size) {
        eprintln!("an RX descriptor table appeared at {off:#x} after the page was overwritten");
        exit(1);
    }
    if let Some(off) = find_tx_desc(region.as_slice()) {
        eprintln!("a TX descriptor appeared at {off:#x} after the page was overwritten");
        exit(1);
    }

    println!(
        "nic dma isolation: no virtqueue in the {} KiB granted (rx {rx_off:#x}, tx {tx_off:#x}); \
         {TRANSMITS} transmits and {received} frames after every byte of it was overwritten",
        REGION_BYTES / 1024,
    );
}

/// Write a frame into the TX buffer and hand it to the device.
///
/// The net header is zeroed and the rest is [`FRAME_BYTE`], which is what
/// `netd`'s own TX token does one field at a time.
fn transmit(nic: &Nic, region: &mut SharedMemory, tx_off: usize, hdr: usize) {
    let tx = &mut region.as_mut_slice()[tx_off..tx_off + FRAME_LEN];
    tx[..hdr].fill(0);
    tx[hdr..].fill(FRAME_BYTE);
    nic.tx(FRAME_LEN as u64).expect("a NIC claim takes a frame no longer than its TX buffer");
}

/// One descriptor's `addr`, `len` and `flags`, read out of sixteen bytes the
/// way the device reads them.
fn desc(bytes: &[u8]) -> (u64, u32, u16) {
    (
        u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        u16::from_le_bytes(bytes[12..14].try_into().unwrap()),
    )
}

/// Where a run of `count` RX descriptors starts, or `None`.
///
/// The whole run, because one descriptor-shaped sixteen bytes is something
/// frame data could be and `count` consecutive ones is not. Every RX descriptor
/// this driver posts is a one-element chain of exactly `rx_size` writable bytes
/// at a non-zero physical address, so the run is what the table looks like at
/// rest — which is when it matters, since that is when the device has not read
/// it yet.
fn find_rx_table(page: &[u8], count: usize, rx_size: usize) -> Option<usize> {
    let mut run = 0usize;
    for (i, bytes) in page.chunks_exact(DESC_BYTES).enumerate() {
        let (addr, len, flags) = desc(bytes);
        if addr != 0 && len as usize == rx_size && flags & DESC_F_WRITE != 0 {
            run += 1;
            if run == count {
                return Some((i + 1 - count) * DESC_BYTES);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Where the descriptor this test's own transmit wrote is, or `None`.
///
/// A readable one-element chain of exactly [`FRAME_LEN`] bytes: `len` is the
/// fingerprint, and the flags say the device reads it rather than writes it.
fn find_tx_desc(page: &[u8]) -> Option<usize> {
    page.chunks_exact(DESC_BYTES).position(|bytes| {
        let (addr, len, flags) = desc(bytes);
        addr != 0 && len as usize == FRAME_LEN && flags == 0
    })
    .map(|i| i * DESC_BYTES)
}
