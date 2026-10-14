//! The T14's I219, judged on what `toyos-i219` rests on where its interrupts
//! are concerned, on one claim: a capability list with MSI and no MSI-X,
//! causes forced through `ICS` counted on the claim's vector unmasked and
//! masked, an asserted read of `ICR` under auto-mask, and then the driver
//! itself under traffic. Every reading is printed; the exit is whether each
//! held.
//!
//! The PCH datasheet (631120 §8.1) and the 82574's (§10.2.4) are what each
//! reading is held to; no other machine has this function.

use std::rc::Rc;
use std::time::Duration;

use toyos::endow::Endowments;
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos::volatile::Window;
use toyos::PciDev;
use toyos_abi::syscall::{PciId, RegWidth, SyscallError, SYSCAP_LABEL};
use toyos_device_memory::Registers;
use toyos_i219::regs::{self, cause, ctrl_ext};
use toyos_i219::{Clock, Delivery, Interrupts, Part, I219};
use toyos_pci::caps::CapWalk;
use toyos_pci_claim::{Bar, Grant};

const I219_ID: PciId = PciId { vendor: 0x8086, device: 0x15fc };

/// The receive sequence error, bit 3, which the 82574's §10.2.4.1 calls
/// reserved and a PCH part raises: forced beside the link change so the
/// count is of two causes and not one.
const RXSEQ: u32 = 1 << 3;
/// Times each cause is forced unmasked.
const FORCES: u32 = 4;
/// How long a forced cause is given to arrive, and a masked one to stay away.
const WINDOW_NANOS: u64 = toyos_i219::DELIVERY_DEADLINE_NANOS;
/// How long the link is given to come up after the driver's reset.
const LINK_NANOS: u64 = 15_000_000_000;
/// How long frames are listened for after the DHCP discover.
const LISTEN_NANOS: u64 = 3_000_000_000;
/// Frames in the burst: several rings, so the transmit wake is asked for.
const BURST: usize = 4 * toyos_i219::TX_RING;
/// How long the burst and the discover behind it are given to leave: a gigabit
/// link takes them in well under a millisecond.
const SEND_NANOS: u64 = 5_000_000_000;

struct Monotonic;

impl Clock for Monotonic {
    fn nanos(&self) -> u64 {
        toyos_abi::clock::nanos_since_boot()
    }
    fn pause(&self, nanos: u64) {
        std::thread::sleep(Duration::from_nanos(nanos));
    }
}

struct Claim(Rc<PciDev>);

impl Claim {
    fn wait(&self, nanos: u64) {
        let poller = Poller::new(1);
        poller.watch(&*self.0, READABLE, 0);
        poller.wait(1, nanos, |_| {});
    }
}

impl Interrupts for Claim {
    type Refused = SyscallError;
    const IDLE: SyscallError = SyscallError::WouldBlock;
    fn taken(&self) -> Result<u32, SyscallError> {
        self.0.irq().map(|record| record.count)
    }
    fn taken_within(&self, nanos: u64) -> Result<u32, SyscallError> {
        self.wait(nanos);
        self.taken()
    }
}

/// Messages on the claim within `nanos`, 0 for none.
fn messages(claim: &Claim, nanos: u64) -> u32 {
    match claim.taken_within(nanos) {
        Ok(count) => count,
        Err(SyscallError::WouldBlock) => 0,
        Err(why) => fail(&format!("the claim refused its interrupt record: {why:?}")),
    }
}

fn say(what: &str) {
    println!("i219_msi: {what}");
}

fn fail(what: &str) -> ! {
    say(&format!("FAILED: {what}"));
    std::process::exit(1)
}

/// Each capability on the list from `34h`, walked as the kernel walks it:
/// (offset, id, next, Message Control).
fn capabilities(dev: &PciDev) -> Vec<(u8, u8, u8, u16)> {
    let read = |at: u8| {
        dev.config_read(at.into(), RegWidth::U32)
            .unwrap_or_else(|why| fail(&format!("config read at {at:#x} refused: {why:?}")))
    };
    let mut walk = CapWalk::new();
    let mut found = Vec::new();
    let mut next = read(0x34) as u8;
    while let Some(at) = walk.step(next) {
        let header = read(at);
        next = (header >> 8) as u8;
        found.push((at, header as u8, next, (header >> 16) as u16));
    }
    if walk.truncated() {
        fail("the capability list breaks off at a forbidden link");
    }
    found
}

/// Force `forced` with `unmask` in `IMS`, and answer every message that came
/// in the whole window, when the first did, and `ICR` after them; everything
/// is masked and `ICR` cleared again after.
fn force<R: Registers>(regs: &R, claim: &Claim, forced: u32, unmask: u32) -> (u32, u64, u32) {
    regs.write32(regs::IMC, u32::MAX);
    let _ = regs.read32(regs::ICR);
    let _ = claim.taken();
    regs.write32(regs::IMS, unmask);
    let at = Monotonic.nanos();
    regs.write32(regs::ICS, forced);
    let mut count = messages(claim, WINDOW_NANOS);
    let after = Monotonic.nanos() - at;
    // The window is waited out, so a second message for the one cause is
    // counted and not left behind the first.
    while let Some(left) = WINDOW_NANOS.checked_sub(Monotonic.nanos() - at).filter(|left| *left > 0) {
        count += messages(claim, left);
    }
    regs.write32(regs::IMC, u32::MAX);
    let icr = regs.read32(regs::ICR);
    (count, after, icr)
}

/// One read of `ICR` with an interrupt asserted under auto-mask, as every
/// pass of the driver makes it: the read, `IMS` after it, and a second read.
/// §10.2.4.1's case 2 says the read clears `ICR` and writes `IAM` into `IMC`.
fn auto_masked_read<R: Registers>(regs: &R, claim: &Claim) -> (u32, u32, u32) {
    let ext = regs.read32(regs::CTRL_EXT);
    regs.write32(regs::CTRL_EXT, ext | ctrl_ext::IAME);
    regs.write32(regs::IAM, u32::MAX);
    regs.write32(regs::IMC, u32::MAX);
    let _ = regs.read32(regs::ICR);
    let _ = claim.taken();
    regs.write32(regs::IMS, RXSEQ);
    regs.write32(regs::ICS, RXSEQ);
    if messages(claim, WINDOW_NANOS) == 0 {
        fail("a forced cause raised no message for the auto-masked read");
    }
    let first = regs.read32(regs::ICR);
    let mask = regs.read32(regs::IMS);
    let second = regs.read32(regs::ICR);
    regs.write32(regs::IMC, u32::MAX);
    let _ = regs.read32(regs::ICR);
    regs.write32(regs::CTRL_EXT, ext);
    regs.write32(regs::IAM, 0);
    (first, mask, second)
}

/// A DHCP discover from `mac`, broadcast and asking for a broadcast reply
/// (RFC 2131 §4.1, RFC 2132 §9.6): one frame the bench's server answers.
fn dhcp_discover(mac: [u8; 6]) -> Vec<u8> {
    let mut bootp = vec![0u8; 300];
    bootp[..4].copy_from_slice(&[1, 1, 6, 0]);
    bootp[4..8].copy_from_slice(&0x5432_1937u32.to_be_bytes());
    bootp[10] = 0x80;
    bootp[28..34].copy_from_slice(&mac);
    bootp[236..240].copy_from_slice(&[99, 130, 83, 99]);
    bootp[240..244].copy_from_slice(&[53, 1, 1, 255]);
    let udp_len = 8 + bootp.len();
    let ip_len = 20 + udp_len;
    let mut ip = vec![0x45, 0, (ip_len >> 8) as u8, ip_len as u8, 0, 0, 0, 0, 64, 17, 0, 0];
    ip.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 255]);
    let sum = ip.chunks(2).map(|w| u32::from(u16::from_be_bytes([w[0], w[1]]))).sum::<u32>();
    let sum = !((sum & 0xFFFF) + (sum >> 16)) as u16;
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    let mut frame = vec![0xFF; 6];
    frame.extend_from_slice(&mac);
    frame.extend_from_slice(&[0x08, 0x00]);
    frame.extend_from_slice(&ip);
    frame.extend_from_slice(&[0, 68, 0, 67, (udp_len >> 8) as u8, udp_len as u8, 0, 0]);
    frame.extend_from_slice(&bootp);
    frame
}

/// A frame of the IEEE 802 local experimental EtherType to a locally
/// administered address nobody has: what the burst is made of.
fn burst_frame(mac: [u8; 6], n: usize) -> Vec<u8> {
    let mut frame = vec![0x02, 0, 0, 0, 0, 0];
    frame.extend_from_slice(&mac);
    frame.extend_from_slice(&[0x88, 0xB5]);
    frame.resize(64, n as u8);
    frame
}

/// What the passes under traffic saw: messages that came with each cause.
#[derive(Default)]
struct Tally {
    passes: u32,
    rxt0: u32,
    txdw: u32,
    lsc: u32,
    eccer: u32,
}

type Driver = I219<Bar, Monotonic, Grant, Claim>;

fn pass(driver: &mut Driver, tally: &mut Tally) {
    let pass = driver.begin_pass().unwrap_or_else(|why| fail(&format!("a pass was refused: {why}")));
    if pass.messages == 0 {
        return;
    }
    tally.passes += 1;
    let has = |bit: u32| u32::from(pass.causes & bit != 0);
    tally.rxt0 += has(cause::RXT0);
    tally.txdw += has(cause::TXDW);
    tally.lsc += has(cause::LSC);
    tally.eccer += has(cause::ECCER);
}

/// Put `bytes` in a transmit buffer through `frames`, the grant's own window,
/// and hand it to the part.
fn send(driver: &mut Driver, frames: &Window, bytes: &[u8]) {
    let slot = driver.tx_reserve(bytes.len()).unwrap_or_else(|| fail("a ring with room refused a frame"));
    let window = frames.sub(slot.at, bytes.len());
    // SAFETY: the window is inside the grant, which outlives the driver; the
    // part does not read this buffer until `tx_commit` publishes its
    // descriptor, after the copy.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), window.as_ptr(), bytes.len()) };
    driver.tx_commit(slot);
}

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("the test estate is endowed a device-minting capability");
    let dev = Rc::new(cap.claim_pci::<PciDev>(I219_ID).unwrap_or_else(|why| fail(&format!("the claim was refused: {why:?}"))));
    let info = dev.describe().unwrap_or_else(|why| fail(&format!("the description was refused: {why:?}")));
    let (index, bytes) = info
        .bar_bytes
        .iter()
        .enumerate()
        .find(|(_, bytes)| **bytes >= regs::REGISTER_BYTES as u64)
        .map(|(index, bytes)| (index as u32, *bytes))
        .unwrap_or_else(|| fail("no BAR holds the register file"));

    let list = capabilities(&dev);
    for (at, id, next, control) in &list {
        say(&format!("capability at {at:#04x}: id {id:#04x}, next {next:#04x}, control {control:#06x}"));
    }
    if list.iter().any(|(_, id, ..)| *id == 0x11) {
        fail("the function publishes an MSI-X capability");
    }
    let delivery = Delivery::of(|at| dev.config_read(at.into(), RegWidth::U32))
        .unwrap_or_else(|why| fail(&format!("no delivery was read off the list: {why}")));
    if delivery != Delivery::Msi {
        fail(&format!("the claim was armed {delivery:?}"));
    }

    // A second mapping of the same BAR, for what is read beside the driver.
    let (regs, _held) = Bar::map(&dev, index, bytes).unwrap_or_else(|why| fail(&format!("{why}")));

    // The grant is what lets the function master the bus, and a message is a
    // write it masters.
    let (driven, _mapping) = Bar::map(&dev, index, bytes).unwrap_or_else(|why| fail(&format!("{why}")));
    toyos_i219::quiesce(&driven);
    let (grant, _region) = Grant::alloc(&dev, toyos_i219::GRANT_BYTES).unwrap_or_else(|why| fail(&format!("{why}")));
    let frames = grant.window();
    let claim = Claim(Rc::clone(&dev));

    let mut held = true;
    for (forced, name) in [(RXSEQ, "RXSEQ"), (cause::LSC, "LSC")] {
        for n in 0..FORCES {
            let (count, after, icr) = force(&regs, &claim, forced, forced);
            say(&format!("{name} forced unmasked ({n}): {count} message(s) in {} us, ICR {icr:#010x}", after / 1000));
            held &= count == 1 && icr & forced == forced;
        }
        let (count, _, icr) = force(&regs, &claim, forced, 0);
        say(&format!("{name} forced masked: {count} message(s) in {} ms, ICR {icr:#010x}", WINDOW_NANOS / 1_000_000));
        held &= count == 0 && icr & forced == forced;
    }
    if !held {
        fail("a forced cause was not exactly one message unmasked and none masked");
    }

    let (first, mask, second) = auto_masked_read(&regs, &claim);
    say(&format!(
        "IAME on: the asserted ICR read {first:#010x}, IMS after it {mask:#010x}, the second read {second:#010x}"
    ));
    if first != cause::INT_ASSERTED | RXSEQ || mask != 0 || second != 0 {
        fail("the asserted read under auto-mask did not clear ICR and mask every cause");
    }

    let mut driver = I219::open(Part::I219, delivery, driven, Monotonic, grant, Claim(Rc::clone(&dev)))
        .unwrap_or_else(|why| fail(&format!("the driver refused the function: {why}")));
    say(&format!("the driver: {}", driver.brought_up().delivered));

    let mut tally = Tally::default();
    let opened = Monotonic.nanos();
    while !driver.link().is_up() {
        if Monotonic.nanos() - opened > LINK_NANOS {
            fail(&format!("no link in {} s", LINK_NANOS / 1_000_000_000));
        }
        claim.wait(LINK_NANOS);
        pass(&mut driver, &mut tally);
    }
    say(&format!("link up {:?} after the driver came up", Duration::from_nanos(Monotonic.nanos() - opened)));

    let mac = driver.mac();
    let sending = Monotonic.nanos();
    let mut sent = 0;
    while sent < BURST {
        if Monotonic.nanos() - sending > SEND_NANOS {
            fail(&format!("{sent} of {BURST} frames left in {} s", SEND_NANOS / 1_000_000_000));
        }
        if driver.wake_on_room() == 0 {
            claim.wait(WINDOW_NANOS);
            pass(&mut driver, &mut tally);
            continue;
        }
        send(&mut driver, &frames, &burst_frame(mac, sent));
        sent += 1;
    }
    let discover = dhcp_discover(mac);
    while driver.tx_room() == 0 {
        if Monotonic.nanos() - sending > SEND_NANOS {
            fail("the ring never had room for the discover");
        }
        claim.wait(WINDOW_NANOS);
        pass(&mut driver, &mut tally);
    }
    send(&mut driver, &frames, &discover);

    let listening = Monotonic.nanos();
    let mut received = 0u32;
    while Monotonic.nanos() - listening < LISTEN_NANOS {
        claim.wait(LISTEN_NANOS - (Monotonic.nanos() - listening).min(LISTEN_NANOS));
        pass(&mut driver, &mut tally);
        while let Some(frame) = driver.poll_rx() {
            received += 1;
            driver.rx_done(frame);
        }
        driver.rx_flush();
    }
    driver.reclaim();
    let (counters, wire) = (driver.counters(), driver.wire());
    let ecc = regs.read32(regs::PBECCSTS);
    say(&format!(
        "traffic: {} pass(es) on a message; with RXT0 {}, TXDW {}, LSC {}, ECCER {}; {received} frame(s) \
         received, {} descriptor(s) written back, the ring full {} time(s), {} wake(s) asked and {} taken, \
         {} stranded; the MAC counted {} sent; PBECCSTS {ecc:#010x}",
        tally.passes,
        tally.rxt0,
        tally.txdw,
        tally.lsc,
        tally.eccer,
        counters.sent,
        counters.tx_full,
        counters.tx_wake_armed,
        counters.tx_wake_taken,
        counters.stranded,
        wire.sent,
    ));

    drop(driver);

    if received == 0 || tally.rxt0 == 0 {
        fail("no frame arrived on a message under traffic");
    }
    if tally.lsc == 0 {
        fail("the link came up and no message carried LSC");
    }
    if counters.tx_full == 0 || counters.tx_wake_taken == 0 {
        fail("the burst never filled the ring, or the transmit wake it asked for never came");
    }
    if counters.sent as u64 != wire.sent || counters.stranded != 0 {
        fail("the descriptors written back are not the frames the MAC sent");
    }
    if ecc & regs::pbeccsts::ECC_ENABLE == 0 {
        fail("the packet buffer's ECC is off, so no ECC error could have been reported");
    }
    if tally.eccer != 0 || ecc & regs::pbeccsts::COUNTS != 0 {
        fail("the packet buffer reported an ECC error");
    }
    say("done");
}
