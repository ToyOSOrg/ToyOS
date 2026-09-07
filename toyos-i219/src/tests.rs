//! The driver's ring, interrupt and link logic, against the specification's
//! own model of the part.
//!
//! Nothing here boots anything. Each test names the clause of the datasheet it
//! is about, and every one that can fail differently from run to run prints the
//! seed that reproduces it.

use std::vec::Vec;
use std::{format, vec};

use crate::regs::{self, cause, ctrl, ivar, rctl, rx_desc, tctl, tx_desc};
use crate::stub::{Nic, Permits, NVM_MAC};
use crate::*;

type Driver = I219<crate::stub::Bar, crate::stub::Ticker, crate::stub::Grant, crate::stub::Line>;

fn open(nic: &Nic) -> Driver {
    let (bar, clock, grant, line) = nic.parts();
    I219::open(bar, clock, grant, line)
        .unwrap_or_else(|why| panic!("{}", nic.because(&format!("open refused it: {why}"))))
}

/// A frame whose bytes name themselves, so a test can tell one from another.
fn frame(tag: u8, len: usize) -> Vec<u8> {
    let mut bytes = vec![tag; len];
    bytes[0] = 0xFF;
    bytes[6] = tag;
    bytes
}

/// Drain one pass, returning what came up and giving every buffer back.
///
/// **Every frame is taken before any buffer goes back**, which is what a
/// caller with a batch of them does — and what stops a driver that reads a
/// buffer before its descriptor said the frame was there from being rescued by
/// the write-back its own `RDT` write provoked.
fn drain(nic: &Nic, driver: &mut Driver) -> Vec<Vec<u8>> {
    let mut got = Vec::new();
    let mut taken = Vec::new();
    while let Some(f) = driver.poll_rx() {
        got.push(nic.bytes(f.at, f.len));
        taken.push(f.index);
    }
    for index in taken {
        driver.rx_done(index);
    }
    got
}

// --- bring-up ---

/// §4.6.5 and §4.6.6 say what a driver programs and §10.2.2.1 says what it may
/// not: `ASDE` "must be set to 0b in the 82574", and the two reserved bits the
/// same section documents as set have to survive a bring-up that read them.
#[test]
fn bring_up_programs_what_the_initialization_sections_name() {
    let nic = Nic::new(1);
    let driver = open(&nic);

    let control = nic.peek(regs::CTRL);
    assert!(control & ctrl::SLU != 0, "{}", nic.because("CTRL.SLU was not set"));
    for (bit, name) in [
        (ctrl::ASDE, "ASDE"),
        (ctrl::ILOS, "ILOS"),
        (ctrl::FRCSPD, "FRCSPD"),
        (ctrl::FRCDPLX, "FRCDPLX"),
        (ctrl::RST, "RST"),
    ] {
        assert!(control & bit == 0, "{}", nic.because(&format!("CTRL.{name} was left set")));
    }
    assert!(
        control & (1 << 3) != 0 && control & (1 << 20) != 0,
        "{}",
        nic.because("a reserved CTRL bit the datasheet says must stay set was cleared")
    );

    assert_eq!(
        nic.peek(regs::RCTL),
        rctl::EN | rctl::BAM | rctl::SECRC | rctl::BSIZE_2048,
        "{}",
        nic.because("RCTL is not what §4.6.5 and §10.2.5.1 describe")
    );
    assert_eq!(
        nic.peek(regs::TCTL),
        tctl::EN | tctl::PSP | tctl::CT | tctl::COLD_FULL_DUPLEX,
        "{}",
        nic.because("TCTL is not §4.6.6's suggested configuration")
    );
    assert_eq!(nic.peek(regs::TIPG), regs::TIPG_DEFAULT);
    assert_eq!(nic.peek(regs::TXDCTL), regs::txdctl::SUGGESTED);
    assert_eq!(
        nic.peek(regs::IMS),
        cause::ENABLED | cause::ENABLED_MSIX,
        "{}",
        nic.because(
            "§4.6.5 names RXT, RXO, RXDMT and LSC and no transmit cause, and §10.2.4.9's \
             vectored names go beside them on a part that has IVAR"
        )
    );
    assert_eq!(
        nic.peek(regs::EIAC),
        0,
        "{}",
        nic.because("§10.2.4.7 says ICR must not be read while EIAC has bits set")
    );
    // §7.1.8: `RDLEN` "must be a multiple of 128", and the tail points one
    // descriptor beyond the end.
    assert_eq!(nic.peek(regs::RDLEN) as usize, RX_RING * rx_desc::BYTES);
    assert_eq!(nic.peek(regs::RDLEN) % 128, 0);
    assert_eq!(nic.peek(regs::RDT) as usize, RX_RING - 1);
    assert_eq!(nic.peek(regs::TDLEN) as usize, TX_RING * tx_desc::BYTES);
    assert_eq!(nic.peek(regs::TDLEN) % 128, 0);
    assert_eq!(nic.peek(regs::TDT), 0);

    // Both timers off: an interrupt this driver waits on may not be held back
    // by one nothing else expires.
    assert_eq!(nic.peek(regs::RDTR), 0);
    assert_eq!(nic.peek(regs::RADV), 0);
    assert_eq!(nic.peek(regs::ITR), 0);

    assert_eq!(driver.mac(), NVM_MAC);
}

/// §10.2.5.23: "If no NVM is present the Address Valid field for n=0b will be
/// 0b" — and a part with no station address passes nothing through its filter,
/// so it is refused by name rather than driven into silence.
#[test]
fn a_part_with_no_station_address_is_refused() {
    let nic = Nic::new(2);
    nic.without_nvm();
    let (bar, clock, grant, line) = nic.parts();
    assert_eq!(I219::open(bar, clock, grant, line).err(), Some(Refusal::NoStationAddress));
}

/// The two bounds every later access rests on, refused before anything is
/// touched.
#[test]
fn a_window_or_grant_too_small_is_refused() {
    struct Narrow(usize);
    impl Registers for Narrow {
        fn len(&self) -> usize {
            self.0
        }
        fn read(&self, _: usize) -> u32 {
            panic!("a refused window was read")
        }
        fn write(&self, _: usize, _: u32) {
            panic!("a refused window was written")
        }
    }
    struct NoClock;
    impl Clock for NoClock {
        fn nanos(&self) -> u64 {
            0
        }
    }
    struct Small(usize);
    impl DmaBuffers for Small {
        fn len(&self) -> usize {
            self.0
        }
        fn device_addr(&self, _: usize) -> u64 {
            panic!("a refused grant was addressed")
        }
        fn read(&self, _: usize) -> u64 {
            panic!("a refused grant was read")
        }
        fn write(&self, _: usize, _: u64) {
            panic!("a refused grant was written")
        }
        fn publish(&self) {}
        fn observe(&self) {}
    }
    struct NoIrq;
    impl Interrupts for NoIrq {
        fn taken(&self) -> u32 {
            0
        }
    }

    let narrow = I219::open(
        Narrow(regs::REGISTER_BYTES - 4),
        NoClock,
        Small(GRANT_BYTES as usize),
        NoIrq,
    );
    assert_eq!(
        narrow.err(),
        Some(Refusal::Window {
            given: regs::REGISTER_BYTES - 4,
            needed: regs::REGISTER_BYTES
        })
    );

    let nic = Nic::new(3);
    let (bar, clock, _, line) = nic.parts();
    let short = I219::open(bar, clock, Small(GRANT_BYTES as usize - 1), line);
    assert_eq!(
        short.err(),
        Some(Refusal::Grant {
            given: GRANT_BYTES as usize - 1,
            needed: GRANT_BYTES as usize
        })
    );
}

/// A window nothing routes answers ones on every access. A driver that went on
/// would read `ff:ff:ff:ff:ff:ff` as its own address and never say why.
#[test]
fn a_window_that_reads_ones_is_refused() {
    struct Dead;
    impl Registers for Dead {
        fn len(&self) -> usize {
            regs::REGISTER_BYTES
        }
        fn read(&self, _: usize) -> u32 {
            u32::MAX
        }
        fn write(&self, _: usize, _: u32) {}
    }
    struct NoClock;
    impl Clock for NoClock {
        fn nanos(&self) -> u64 {
            0
        }
    }
    struct NoIrq;
    impl Interrupts for NoIrq {
        fn taken(&self) -> u32 {
            0
        }
    }
    let nic = Nic::new(4);
    let (_, _, grant, _) = nic.parts();
    assert_eq!(I219::open(Dead, NoClock, grant, NoIrq).err(), Some(Refusal::Dead));
}

// --- what a written-back descriptor must satisfy ---

/// The bit arithmetic on its own, driven with words no working part would
/// write. Each arm is a way to make this driver act on a number the device
/// chose.
mod refusals {
    use super::*;

    fn word(len: u32, status: u8, errors: u8) -> u64 {
        (len as u64 & rx_desc::LENGTH_MASK)
            | ((status as u64) << rx_desc::STATUS_SHIFT)
            | ((errors as u64) << rx_desc::ERRORS_SHIFT)
    }

    const DONE: u8 = rx_desc::status::DD | rx_desc::status::EOP;

    /// The one that matters most: this number becomes the length of a slice
    /// handed to smoltcp, so a device claiming more than the buffer holds
    /// would be a read past it.
    #[test]
    fn more_bytes_than_the_buffer_holds_is_refused() {
        assert_eq!(parse_rx(word(RX_BUF_BYTES as u32, DONE, 0)), Ok(RX_BUF_BYTES));
        assert_eq!(
            parse_rx(word(RX_BUF_BYTES as u32 + 1, DONE, 0)),
            Err(RxRefusal::Length { len: RX_BUF_BYTES as u32 + 1 })
        );
        assert_eq!(parse_rx(word(0xFFFF, DONE, 0)), Err(RxRefusal::Length { len: 0xFFFF }));
    }

    /// §7.1.3.4: the error bits are "valid only when the EOP and DD bits are
    /// set", and any of the ones about the frame's own bytes drops it.
    #[test]
    fn a_frame_the_hardware_reported_an_error_on_is_refused() {
        for bit in [
            rx_desc::errors::CE,
            rx_desc::errors::SE,
            rx_desc::errors::SEQ,
            rx_desc::errors::CXE,
            rx_desc::errors::RXE,
        ] {
            assert_eq!(parse_rx(word(64, DONE, bit)), Err(RxRefusal::Errors { errors: bit }));
        }
        // The two checksum bits are not about the frame's bytes and this
        // driver asks for no offload, so a device that sets them anyway is not
        // a reason to drop a frame.
        assert_eq!(parse_rx(word(64, DONE, rx_desc::errors::IPE)), Ok(64));
        assert_eq!(parse_rx(word(64, DONE, rx_desc::errors::TCPE)), Ok(64));
    }

    /// §7.1.3.3: "If EOP is not set for a descriptor, only the Address,
    /// Length, and DD bits are valid" — so nothing else in it is read, and
    /// with `RCTL.LPE` clear no frame should have been split at all.
    #[test]
    fn a_descriptor_without_end_of_packet_is_refused() {
        assert_eq!(parse_rx(word(1500, rx_desc::status::DD, 0)), Err(RxRefusal::Split));
        // And the errors byte is not consulted on the way to that answer.
        assert_eq!(
            parse_rx(word(1500, rx_desc::status::DD, rx_desc::errors::RXE)),
            Err(RxRefusal::Split)
        );
    }

    /// §7.1.7.2's null descriptor padding writes back `DD` "and all other bits
    /// unchanged", and a frame shorter than its own header is nothing to hand
    /// up either.
    #[test]
    fn a_descriptor_with_nothing_in_it_is_refused() {
        assert_eq!(parse_rx(word(0, DONE, 0)), Err(RxRefusal::Empty { len: 0 }));
        assert_eq!(parse_rx(word(13, DONE, 0)), Err(RxRefusal::Empty { len: 13 }));
        assert_eq!(parse_rx(word(14, DONE, 0)), Ok(14));
    }
}

// --- receive ---

#[test]
fn a_frame_arrives_whole() {
    let nic = Nic::new(5);
    let mut driver = open(&nic);
    nic.set_link(true);
    let sent = frame(0xA5, 300);
    nic.deliver(&sent);
    nic.run();
    driver.begin_pass();
    assert_eq!(drain(&nic, &mut driver), vec![sent]);
}

/// §7.1.7.1 lets the device write descriptors back in chunks and in the order
/// it likes inside one; §7.1.8 makes the head register a shadow that counts
/// descriptors "not yet stored in memory". Both are on here, so a driver that
/// took `RDH` for a completion count, or that scanned the ring for the highest
/// `DD` it could find, hands up buffers the device has not written.
#[test]
fn frames_come_up_in_order_under_batched_and_reordered_write_back() {
    for seed in 1..=64u64 {
        let nic = Nic::new(seed);
        let mut driver = open(&nic);
        nic.set_link(true);
        let sent: Vec<Vec<u8>> = (0..40).map(|i| frame(i as u8 + 1, 100 + i * 7)).collect();
        for f in &sent {
            nic.deliver(f);
        }
        let mut got: Vec<Vec<u8>> = Vec::new();
        // Passes, not one drain: the point is that a pass ends when memory
        // says so and the next one picks up where it left off.
        for _ in 0..64 {
            nic.run();
            driver.begin_pass();
            got.extend(drain(&nic, &mut driver));
            if got.len() == sent.len() {
                break;
            }
        }
        assert_eq!(
            got,
            sent,
            "{}",
            nic.because("the frames did not come up whole and in the order they arrived")
        );
    }
}

/// The premise of the test above: with the shadow head on, the register really
/// does run ahead of what a driver can read out of memory. A model where it
/// never did would make that test pass for the wrong reason.
#[test]
fn the_head_register_does_run_ahead_of_memory() {
    let mut ahead = 0;
    for seed in 1..=64u64 {
        let nic = Nic::new(seed);
        let mut driver = open(&nic);
        nic.set_link(true);
        for i in 0..8 {
            nic.deliver(&frame(i + 1, 200));
        }
        nic.run();
        driver.begin_pass();
        // Counted without giving any buffer back, so the device does not run
        // again underneath the measurement.
        let mut up = 0;
        while driver.poll_rx().is_some() {
            up += 1;
        }
        if nic.peek(regs::RDH) as usize > up {
            ahead += 1;
        }
    }
    assert!(
        ahead > 0,
        "the model never advanced RDH past what it had written back, so the ordering \
         these tests are about was never exercised"
    );
}

/// A device that claims more bytes than the buffer it was given is refused at
/// the descriptor, counted, and its buffer goes straight back — one element
/// this driver will not act on may not hide the ones behind it.
#[test]
fn a_lying_length_is_refused_and_the_frames_behind_it_still_arrive() {
    let nic = Nic::new(6);
    let mut driver = open(&nic);
    nic.set_link(true);
    let first = frame(0x10, 256);
    let behind = frame(0x11, 512);
    nic.deliver(&first);
    nic.deliver(&behind);
    nic.run();
    // The device really did fill descriptor 0; what is rewritten is the number
    // it reported, which is the device's and not this driver's.
    nic.poke_rx_status(
        0,
        0xFFFF
            | (((rx_desc::status::DD | rx_desc::status::EOP) as u64) << rx_desc::STATUS_SHIFT),
    );
    driver.begin_pass();
    let got = drain(&nic, &mut driver);
    assert_eq!(driver.counters().over_length, 1, "{}", nic.because("the length was believed"));
    assert_eq!(got, vec![behind], "{}", nic.because("the frame behind it was lost"));
}

/// §7.1.7.2: a descriptor whose data address is null comes back with `DD` set
/// and nothing else. It is not a frame, and the buffer goes back.
#[test]
fn null_descriptor_padding_is_not_a_frame() {
    let nic = Nic::new(7);
    let mut driver = open(&nic);
    nic.set_link(true);
    nic.null_rx_buffer(0);
    nic.deliver(&frame(0x22, 400));
    nic.run();
    driver.begin_pass();
    let got = drain(&nic, &mut driver);
    // Refused as a split and not as an empty one, because that is all a driver
    // can tell: §7.1.7.2 leaves "all other bits unchanged", so a padding
    // write-back and the first descriptor of a chain are the same word. Both
    // are dropped and both give the buffer back, which is what matters.
    assert_eq!(
        driver.counters().split,
        1,
        "{}",
        nic.because("the padding was taken for a frame")
    );
    assert_eq!(got.len(), 1, "{}", nic.because("the real frame did not come up behind it"));
}

/// **A ring that never empties is what the wire does at line rate.** A pass
/// has to end, or the caller's event loop never runs again — so the budget is
/// what makes "no more this pass" something this driver can say.
#[test]
fn one_pass_hands_up_at_most_its_budget() {
    let nic = Nic::new(8);
    let mut driver = open(&nic);
    nic.set_link(true);
    // Twice the budget, arriving faster than they are taken: every buffer
    // returned is another frame placed.
    for i in 0..(RX_BUDGET as usize * 2) {
        nic.deliver(&frame((i % 200) as u8 + 1, 128));
    }
    // Enough ticks for every one of them to be placed and written back, so
    // what bounds the pass below is the budget and not the device.
    for _ in 0..(RX_BUDGET * 4) {
        nic.run();
    }
    driver.begin_pass();
    let first = drain(&nic, &mut driver).len();
    assert_eq!(
        first, RX_BUDGET as usize,
        "{}",
        nic.because("a pass did not stop at its budget")
    );
    // And the next pass picks the rest up rather than losing them.
    let mut rest = 0;
    for _ in 0..8 {
        nic.run();
        driver.begin_pass();
        rest += drain(&nic, &mut driver).len();
    }
    assert_eq!(first + rest, RX_BUDGET as usize * 2, "{}", nic.because("frames were lost"));
}

/// §7.1.8's tail "identifies the location beyond the last descriptor hardware
/// can process", so it can only ever move over a run of ready descriptors — a
/// buffer returned out of turn would hand the device one that is still being
/// read.
#[test]
#[should_panic(expected = "came back out of turn")]
fn a_buffer_returned_out_of_turn_is_a_bug_and_says_so() {
    let nic = Nic::new(9);
    let mut driver = open(&nic);
    nic.set_link(true);
    nic.deliver(&frame(1, 64));
    nic.deliver(&frame(2, 64));
    nic.run();
    driver.begin_pass();
    let first = driver.poll_rx().expect("a frame");
    let second = driver.poll_rx().expect("a second frame");
    driver.rx_done(second.index);
    driver.rx_done(first.index);
}

// --- transmit ---

#[test]
fn a_frame_goes_out_whole() {
    let nic = Nic::new(10);
    let mut driver = open(&nic);
    nic.set_link(true);
    let payload = frame(0x33, 600);
    let slot = driver.tx_reserve(payload.len()).expect("a free transmit descriptor");
    nic.put_bytes(slot.at, &payload);
    driver.tx_commit(slot);
    nic.run();
    assert_eq!(nic.sent(), vec![payload], "{}", nic.because("the frame did not reach the wire"));
}

/// **A server never blocks.** With every descriptor in flight the next frame is
/// dropped and counted, never waited on: spinning here would park the caller's
/// whole event loop on a device.
#[test]
fn a_full_transmit_ring_drops_rather_than_waits() {
    let nic = Nic::new(11);
    let mut driver = open(&nic);
    nic.set_link(true);
    // The device is held, so nothing is sent, nothing is written back and
    // nothing is reclaimed: the ring really does fill.
    nic.hold();
    // The ring keeps one descriptor in software's hands (§7.2.4: hardware owns
    // `[TDH..TDT)`, so filling the last one would wrap the tail onto the head).
    let mut taken = 0;
    while let Some(slot) = driver.tx_reserve(64) {
        nic.put_bytes(slot.at, &frame(1, 64));
        driver.tx_commit(slot);
        taken += 1;
        if taken > TX_RING * 2 {
            panic!("{}", nic.because("the transmit ring never filled"));
        }
        // Nothing runs, so nothing is written back and nothing is reclaimed.
    }
    assert_eq!(taken, TX_RING - 1, "{}", nic.because("the usable depth is not the ring less one"));
    assert_eq!(driver.tx_free(), 0);
    assert_eq!(driver.counters().tx_dropped, 1);
}

/// §7.2.4.2 lets the device hold transmit write-backs in a batch, so a later
/// descriptor's `DD` says nothing about an earlier one's: the ring is
/// reclaimed from the oldest, one at a time, stopping at the first that is
/// clear.
#[test]
fn transmit_descriptors_are_reclaimed_under_batched_write_back() {
    for seed in 1..=32u64 {
        let nic = Nic::new(seed);
        let mut driver = open(&nic);
        nic.set_link(true);
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut on_the_wire: Vec<Vec<u8>> = Vec::new();
        for i in 0..(TX_RING as u8 * 4) {
            let payload = frame(i + 1, 100 + i as usize);
            // Reclaim happens inside `tx_reserve`, so a ring that is never
            // given back would refuse here rather than send.
            for _ in 0..8 {
                if let Some(slot) = driver.tx_reserve(payload.len()) {
                    nic.put_bytes(slot.at, &payload);
                    driver.tx_commit(slot);
                    out.push(payload.clone());
                    break;
                }
                nic.run();
            }
            nic.run();
            on_the_wire.extend(nic.sent());
        }
        nic.run();
        on_the_wire.extend(nic.sent());
        assert_eq!(
            on_the_wire,
            out,
            "{}",
            nic.because("what reached the wire is not what was published")
        );
        assert_eq!(
            driver.counters().tx_dropped,
            0,
            "{}",
            nic.because("a frame was dropped although the ring was being reclaimed")
        );
    }
}

// --- interrupts and link ---

/// §10.2.4.1's case 3 says a read of `ICR` with no interrupt asserted "has no
/// side affect". A driver that treated the read as the acknowledgement would
/// see the same causes for ever, so the causes it acted on are written back.
#[test]
fn causes_are_acknowledged_by_writing_them_back() {
    let permits = Permits { spurious_interrupts: false, ..Permits::default() };
    let nic = Nic::with(12, permits);
    let mut driver = open(&nic);
    nic.set_link(true);
    driver.begin_pass();

    // A transmit, because §4.6.5 leaves `TXDW` masked: with the cause set and
    // the mask clear, `ICR.INT_ASSERTED` is clear too, which is exactly
    // §10.2.4.1's case 3 — the read that has no side effect. Nothing but the
    // write-back can clear it.
    let payload = frame(0x55, 128);
    let slot = driver.tx_reserve(payload.len()).expect("a free transmit descriptor");
    nic.put_bytes(slot.at, &payload);
    driver.tx_commit(slot);
    nic.run();
    assert_eq!(
        nic.peek(regs::ICR) & cause::INT_ASSERTED,
        0,
        "{}",
        nic.because("a masked cause asserted the interrupt, so case 3 was never reached")
    );
    let pass = driver.begin_pass();
    assert!(
        pass.causes & cause::TXDW != 0,
        "{}",
        nic.because("the transmit completion was never recorded")
    );
    assert_eq!(
        nic.peek(regs::ICR),
        0,
        "{}",
        nic.because("a cause survived the pass that read it, so the read was taken for the \
                     acknowledgement")
    );
    let again = driver.begin_pass();
    assert_eq!(
        again.causes,
        0,
        "{}",
        nic.because("the same causes came back on the next pass")
    );
}

/// §10.2.4.9: `IVAR` "is only valid in MSI-X mode. It defines the allocation
/// of the different interrupt causes to one of the MSI-X vectors." **At reset
/// it allocates none**, so a part in that mode with no `IVAR` programmed fills
/// `ICR` and delivers nothing — which is what QEMU's `e1000e` did before this
/// driver wrote it.
#[test]
fn a_part_in_msi_x_mode_is_told_which_vector_each_cause_uses() {
    let nic = Nic::with(16, Permits { spurious_interrupts: false, ..Permits::default() });
    let mut driver = open(&nic);
    assert!(driver.msix(), "{}", nic.because("the part answered IVAR and was not believed"));
    assert_eq!(
        nic.peek(regs::IVAR),
        ivar::ALL_ON_VECTOR_ZERO,
        "{}",
        nic.because("every cause has to name the one MSI-X entry the kernel programmed")
    );
    driver.begin_pass();

    nic.set_link(true);
    nic.deliver(&frame(0x66, 300));
    nic.run();
    let pass = driver.begin_pass();
    assert!(
        pass.messages > 0,
        "{}",
        nic.because("a frame arrived and the part raised nothing at all")
    );
    assert!(pass.causes & cause::RXQ0 != 0, "{}", nic.because("no receive-queue cause"));
    assert_eq!(drain(&nic, &mut driver).len(), 1);
}

/// The other part: MSI and no `IVAR` at all, which is what the T14's I219 is.
/// A write to the register is dropped, a read answers zero, and the classic
/// causes §4.6.5 names drive the one message directly.
#[test]
fn a_part_with_no_vector_allocation_still_raises_its_interrupt() {
    let nic = Nic::with(17, Permits { spurious_interrupts: false, ..Permits::default() });
    nic.without_msix();
    let mut driver = open(&nic);
    assert!(!driver.msix(), "{}", nic.because("a part with no IVAR was taken for one with"));
    assert_eq!(nic.peek(regs::IVAR), 0);
    assert_eq!(
        nic.peek(regs::IMS),
        cause::ENABLED,
        "{}",
        nic.because("a part with no vectors was masked in a vector's name")
    );
    driver.begin_pass();

    nic.set_link(true);
    nic.deliver(&frame(0x77, 300));
    nic.run();
    let pass = driver.begin_pass();
    assert!(
        pass.messages > 0,
        "{}",
        nic.because("a frame arrived and the part raised nothing at all")
    );
    assert!(pass.causes & cause::RXT0 != 0, "{}", nic.because("no receive-timer cause"));
    assert_eq!(drain(&nic, &mut driver).len(), 1);
}

/// §7.4.5 names the spurious interrupt: a message whose cause is already gone.
/// It costs a pass and nothing else.
#[test]
fn a_spurious_interrupt_costs_a_pass_and_nothing_else() {
    // The model's own dice are off, so the one message below is the only one
    // and the count it is asserted against is exact.
    let nic = Nic::with(13, Permits { spurious_interrupts: false, ..Permits::default() });
    let mut driver = open(&nic);
    nic.set_link(true);
    driver.begin_pass();
    // Every cause already clear, and a message all the same.
    nic.spurious();
    let pass = driver.begin_pass();
    assert_eq!(pass.messages, 1, "{}", nic.because("the message was not delivered"));
    assert_eq!(pass.causes & !cause::INT_ASSERTED, 0);
    assert_eq!(driver.counters().spurious, 1);
    assert_eq!(drain(&nic, &mut driver).len(), 0);
    // And the driver still works afterwards.
    let sent = frame(0x44, 200);
    nic.deliver(&sent);
    nic.run();
    driver.begin_pass();
    assert_eq!(drain(&nic, &mut driver), vec![sent]);
}

/// §10.2.4.1: `LSC` "is set whenever the link status changes (either from up to
/// down, or from down to up)", and §10.2.2.2's `LU` is what it means.
#[test]
fn the_link_going_away_and_coming_back_is_seen() {
    let nic = Nic::new(14);
    let mut driver = open(&nic);
    assert!(!driver.link().up, "{}", nic.because("the link was up before anything plugged in"));

    nic.set_link(true);
    let up = driver.begin_pass();
    assert!(up.link_changed && driver.link().up);
    assert_eq!(driver.link().speed_mbps, 1000);
    assert!(driver.link().full_duplex);
    let at = driver.link_up_after_nanos().expect("a link-up time");

    nic.set_link(false);
    let down = driver.begin_pass();
    assert!(down.causes & cause::LSC != 0, "{}", nic.because("no LSC for the link going away"));
    assert!(down.link_changed && !driver.link().up);
    assert_eq!(driver.link().speed_mbps, 0);

    nic.set_link(true);
    driver.begin_pass();
    assert!(driver.link().up);
    // The first time it came up is the one the profile measures, so a drop and
    // a recovery may not move it.
    assert_eq!(driver.link_up_after_nanos(), Some(at));
}

/// A frame that arrives while the link is down and comes up after it is still
/// the same frame: nothing in the receive path is gated on the link.
#[test]
fn a_link_drop_does_not_lose_the_ring() {
    let nic = Nic::new(15);
    let mut driver = open(&nic);
    nic.set_link(true);
    let first = frame(1, 100);
    nic.deliver(&first);
    nic.run();
    driver.begin_pass();
    assert_eq!(drain(&nic, &mut driver), vec![first]);

    nic.set_link(false);
    driver.begin_pass();
    nic.set_link(true);
    driver.begin_pass();

    let second = frame(2, 700);
    nic.deliver(&second);
    nic.run();
    driver.begin_pass();
    assert_eq!(
        drain(&nic, &mut driver),
        vec![second],
        "{}",
        nic.because("the ring did not survive the link going away and coming back")
    );
}

/// Everything at once, over many seeds: frames in both directions, the link
/// flapping, spurious messages, and a ring that keeps refilling. What is
/// asserted is that nothing is lost, nothing is duplicated and nothing is
/// invented.
#[test]
fn a_seeded_workload_loses_nothing_and_invents_nothing() {
    for seed in 1..=48u64 {
        let nic = Nic::new(seed);
        let mut driver = open(&nic);
        nic.set_link(true);
        let mut expect_in: Vec<Vec<u8>> = Vec::new();
        let mut expect_out: Vec<Vec<u8>> = Vec::new();
        let mut got_in: Vec<Vec<u8>> = Vec::new();
        let mut got_out: Vec<Vec<u8>> = Vec::new();

        for round in 0..40u8 {
            let inbound = frame(round + 1, 64 + round as usize * 13);
            nic.deliver(&inbound);
            expect_in.push(inbound);
            if round % 5 == 3 {
                nic.set_link(round % 10 == 3);
            }
            let outbound = frame(0x80 | round, 90 + round as usize * 11);
            if let Some(slot) = driver.tx_reserve(outbound.len()) {
                nic.put_bytes(slot.at, &outbound);
                driver.tx_commit(slot);
                expect_out.push(outbound);
            }
            nic.run();
            driver.begin_pass();
            got_in.extend(drain(&nic, &mut driver));
            got_out.extend(nic.sent());
        }
        for _ in 0..64 {
            nic.run();
            driver.begin_pass();
            got_in.extend(drain(&nic, &mut driver));
            got_out.extend(nic.sent());
        }
        assert_eq!(got_in, expect_in, "{}", nic.because("the receive path lost or reordered"));
        assert_eq!(got_out, expect_out, "{}", nic.because("the transmit path lost or reordered"));
        let counters = driver.counters();
        assert_eq!(counters.over_length, 0, "{}", nic.because("a length was refused"));
        assert_eq!(counters.errored, 0, "{}", nic.because("a frame was reported bad"));
        assert_eq!(counters.split, 0, "{}", nic.because("a frame was split"));
    }
}
