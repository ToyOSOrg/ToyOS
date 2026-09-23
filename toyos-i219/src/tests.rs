//! The driver's ring, interrupt and link logic, against the specification's
//! own model of the part.
//!
//! Nothing here boots anything. Each test names the clause of the datasheet it
//! is about, and every one that can fail differently from run to run prints the
//! seed that reproduces it.

use std::vec::Vec;
use std::{format, vec};

use crate::phy as toyos_phy;
use crate::phy::{Others, Phy, PhyRefusal};
use crate::regs::{self, cause, ctrl, extcnf, ivar, rctl, rx_desc, tctl, tx_desc};
use crate::stub::{Nic, Permits, Unanswered, NVM_MAC};
use crate::*;

type Driver = I219<crate::stub::Bar, crate::stub::Ticker, crate::stub::Grant, crate::stub::Line>;

fn open(nic: &Nic) -> Driver {
    let (bar, clock, grant, line) = nic.parts();
    I219::open(nic.part(), bar, clock, grant, line)
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
        taken.push(f);
    }
    for frame in taken {
        driver.rx_done(frame);
    }
    got
}

/// One pass, with the claim answering. A claim that refuses is not a pass with
/// nothing in it, and a test that read one as such would be measuring silence.
fn one_pass(driver: &mut Driver) -> Pass {
    driver.begin_pass().expect("the claim answered its interrupt record")
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
        // §3.1.3.10: left set, the function issues no master request at all,
        // so its rings are descriptors nothing fetches.
        (ctrl::GIO_MASTER_DISABLE, "GIO_MASTER_DISABLE"),
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
             vectored names go beside them"
        )
    );
    assert_eq!(
        nic.peek(regs::EIAC),
        0,
        "{}",
        nic.because("§10.2.4.7 says ICR must not be read while EIAC has bits set")
    );
    assert_eq!(nic.peek(regs::RDLEN) as usize, RX_RING * rx_desc::BYTES);
    // §4.6.5.1: "the tail pointer should be set to point one descriptor beyond
    // the end".
    assert_eq!(nic.peek(regs::RDT) as usize, RX_RING - 1);
    assert_eq!(nic.peek(regs::TDLEN) as usize, TX_RING * tx_desc::BYTES);
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
    assert_eq!(
        I219::open(nic.part(), bar, clock, grant, line).err(),
        Some(Refusal::NoStationAddress)
    );
}

/// The two bounds every later access rests on, refused before anything is
/// touched.
#[test]
fn a_window_or_grant_too_small_is_refused() {
    struct Narrow(usize);
    impl Registers for Narrow {
        fn bytes(&self) -> usize {
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
        fn pause(&self, _: u64) {
            panic!("a refused window waited")
        }
    }
    struct Small(usize);
    impl DmaBuffers for Small {
        fn bytes(&self) -> usize {
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
        type Refused = ();
        const IDLE: () = ();
        fn taken(&self) -> Result<u32, ()> {
            Err(())
        }
    }

    let narrow = I219::open(
        Part::E82574,
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
    let short = I219::open(nic.part(), bar, clock, Small(GRANT_BYTES as usize - 1), line);
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
        fn bytes(&self) -> usize {
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
        fn pause(&self, _: u64) {
            panic!("a refused window waited")
        }
    }
    struct NoIrq;
    impl Interrupts for NoIrq {
        type Refused = ();
        const IDLE: () = ();
        fn taken(&self) -> Result<u32, ()> {
            Err(())
        }
    }
    let nic = Nic::new(4);
    let (_, _, grant, _) = nic.parts();
    assert_eq!(
        I219::open(Part::E82574, Dead, NoClock, grant, NoIrq).err(),
        Some(Refusal::Dead)
    );
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
    one_pass(&mut driver);
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
            one_pass(&mut driver);
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
        one_pass(&mut driver);
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
    one_pass(&mut driver);
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
    one_pass(&mut driver);
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
    one_pass(&mut driver);
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
        one_pass(&mut driver);
        rest += drain(&nic, &mut driver).len();
    }
    assert_eq!(first + rest, RX_BUDGET as usize * 2, "{}", nic.because("frames were lost"));
}

/// §7.1.8's tail "identifies the location beyond the last descriptor hardware
/// can process", so it moves over a run of ready descriptors and never to the
/// index that came back last. A buffer given back before an older one may not
/// carry the tail over the older one — whose bytes its holder is still reading
/// — nor, being the lower index, walk `RDT` backwards over the rest of the
/// ring.
#[test]
fn a_buffer_given_back_out_of_turn_does_not_carry_the_tail_over_an_older_one() {
    let nic = Nic::with(
        9,
        Part::E82574,
        Permits { batched_writeback: false, spurious_interrupts: false, ..Permits::default() },
    );
    let mut driver = open(&nic);
    nic.set_link(true);
    nic.deliver(&frame(1, 64));
    nic.deliver(&frame(2, 64));
    nic.run();
    one_pass(&mut driver);
    let first = driver.poll_rx().expect("a frame");
    let second = driver.poll_rx().expect("a second frame");
    let armed = nic.peek(regs::RDT);
    assert_eq!(armed as usize, RX_RING - 1, "{}", nic.because("the ring did not arm its tail"));

    driver.rx_done(second);
    assert_eq!(
        nic.peek(regs::RDT),
        armed,
        "{}",
        nic.because("the tail moved over a descriptor whose frame was still held")
    );

    driver.rx_done(first);
    assert_eq!(
        nic.peek(regs::RDT),
        1,
        "{}",
        nic.because("the tail did not move over both buffers once both were back")
    );
}

/// The same rule on the driver's own path: a descriptor it refuses is given
/// back where it stands, and that may not carry the tail over a frame the
/// caller is still holding the receipt for.
#[test]
fn a_refused_descriptor_does_not_carry_the_tail_over_a_frame_still_held() {
    let nic = Nic::with(
        23,
        Part::E82574,
        Permits { batched_writeback: false, spurious_interrupts: false, ..Permits::default() },
    );
    let mut driver = open(&nic);
    nic.set_link(true);
    nic.deliver(&frame(1, 64));
    nic.deliver(&frame(2, 64));
    nic.run();
    // The device really did fill descriptor 1; what is rewritten is the number
    // it reported, which is the device's and not this driver's.
    nic.poke_rx_status(
        1,
        0xFFFF
            | (((rx_desc::status::DD | rx_desc::status::EOP) as u64) << rx_desc::STATUS_SHIFT),
    );
    one_pass(&mut driver);
    let held = driver.poll_rx().expect("the first frame");
    assert!(
        driver.poll_rx().is_none(),
        "{}",
        nic.because("the refused descriptor was handed up as a frame")
    );
    assert_eq!(driver.counters().over_length, 1, "{}", nic.because("the length was believed"));
    assert_eq!(
        nic.peek(regs::RDT) as usize,
        RX_RING - 1,
        "{}",
        nic.because("giving a refused descriptor back carried the tail over the frame behind it")
    );

    driver.rx_done(held);
    assert_eq!(
        nic.peek(regs::RDT),
        1,
        "{}",
        nic.because("the tail did not move over both once the held frame came back")
    );
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

/// §10.2.4.4: writing a cause to `ICS` sets it in `ICR` as if the event had
/// happened, so a driver can make the part raise a message nothing on the wire
/// caused. That is the whole of the delivery experiment: a claim that takes
/// this message has an interrupt path, whatever the link is doing.
#[test]
fn a_cause_written_to_ics_raises_a_message_the_claim_takes() {
    for part in [Part::E82574, Part::I219] {
        let nic =
            Nic::with(23, part, Permits { spurious_interrupts: false, ..Permits::default() });
        let mut driver = open(&nic);
        // The bring-up's own causes first, so what the pass below reads is the
        // provoked one and not what `open` left standing.
        one_pass(&mut driver);

        driver.provoke_message();
        let pass = one_pass(&mut driver);
        assert_eq!(
            pass.messages, 1,
            "{}",
            nic.because("one cause written to ICS once is one message")
        );
        // `OTHER` is §10.2.4.1's summary of `LSC` and is set with it; nothing
        // else was written, so nothing else may be read.
        assert_eq!(
            pass.causes & !cause::INT_ASSERTED,
            cause::LSC | cause::OTHER,
            "{}",
            nic.because("the causes read are not exactly the one written")
        );
    }
}

/// The negative control for the test above: the same boot with nothing written
/// to `ICS` takes no message, so a green arm there is the write's doing and not
/// a part that speaks on its own.
#[test]
fn a_pass_with_no_provoked_cause_takes_no_message() {
    for part in [Part::E82574, Part::I219] {
        let nic =
            Nic::with(23, part, Permits { spurious_interrupts: false, ..Permits::default() });
        let mut driver = open(&nic);
        one_pass(&mut driver);

        let pass = one_pass(&mut driver);
        assert_eq!(
            pass.messages, 0,
            "{}",
            nic.because("a message arrived on a pass nothing asked the part for one")
        );
    }
}

/// §10.2.4.1's case 3 says a read of `ICR` with no interrupt asserted "has no
/// side affect". A driver that treated the read as the acknowledgement would
/// see the same causes for ever, so the causes it acted on are written back.
#[test]
fn causes_are_acknowledged_by_writing_them_back() {
    let permits = Permits { spurious_interrupts: false, ..Permits::default() };
    let nic = Nic::with(12, Part::E82574, permits);
    let mut driver = open(&nic);
    nic.set_link(true);
    one_pass(&mut driver);

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
    let pass = one_pass(&mut driver);
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
    let again = one_pass(&mut driver);
    assert_eq!(
        again.causes,
        0,
        "{}",
        nic.because("the same causes came back on the next pass")
    );
}

/// §10.2.4.9: `IVAR` "is only valid in MSI-X mode. It defines the allocation
/// of the different interrupt causes to one of the MSI-X vectors." **At reset
/// it allocates none**, so a part with it unprogrammed fills `ICR` and delivers
/// nothing.
#[test]
fn a_part_is_told_which_vector_each_cause_uses() {
    let nic = Nic::with(16, Part::E82574, Permits { spurious_interrupts: false, ..Permits::default() });
    let mut driver = open(&nic);
    assert_eq!(
        nic.peek(regs::IVAR),
        ivar::ALL_ON_VECTOR_ZERO,
        "{}",
        nic.because("every cause has to name the one MSI-X entry the kernel programmed")
    );
    one_pass(&mut driver);

    nic.set_link(true);
    nic.deliver(&frame(0x66, 300));
    nic.run();
    let pass = one_pass(&mut driver);
    assert!(
        pass.messages > 0,
        "{}",
        nic.because("a frame arrived and the part raised nothing at all")
    );
    assert!(pass.causes & cause::RXQ0 != 0, "{}", nic.because("no receive-queue cause"));
    assert_eq!(drain(&nic, &mut driver).len(), 1);
}

/// §10.2.4.9 defines `IVAR` only "in MSI-X mode" and says nothing about what a
/// part outside that mode answers, so a part that does not take the write is
/// refused by name — this driver does not guess which interrupt such a part
/// would raise instead.
#[test]
fn a_part_that_does_not_take_ivar_is_refused() {
    let nic = Nic::new(17);
    nic.refuses_writes_to(regs::IVAR);
    let (bar, clock, grant, line) = nic.parts();
    assert_eq!(
        I219::open(nic.part(), bar, clock, grant, line).err(),
        Some(Refusal::NotAccepted {
            reg: regs::IVAR,
            wrote: ivar::ALL_ON_VECTOR_ZERO,
            read: 0
        })
    );
}

/// The receiver is the last register §4.6.5.1 has a driver write, and a window
/// that takes everything before it and not that is not this register file.
#[test]
fn a_part_that_does_not_take_rctl_is_refused() {
    let nic = Nic::new(18);
    nic.refuses_writes_to(regs::RCTL);
    let (bar, clock, grant, line) = nic.parts();
    assert_eq!(
        I219::open(nic.part(), bar, clock, grant, line).err(),
        Some(Refusal::NotAccepted {
            reg: regs::RCTL,
            wrote: rctl::EN | rctl::BAM | rctl::SECRC | rctl::BSIZE_2048,
            read: 0
        })
    );
}

/// §10.2.2.1 says `CTRL.RST` "is self-clearing" and gives no time, so the
/// deadline is this driver's own — and a part that never clears it is refused
/// rather than spun on for the boot.
#[test]
fn a_reset_that_never_finishes_is_refused() {
    let nic = Nic::new(19);
    nic.reset_never_clears();
    let (bar, clock, grant, line) = nic.parts();
    let Some(Refusal::ResetUnfinished { after_nanos }) =
        I219::open(nic.part(), bar, clock, grant, line).err()
    else {
        panic!("{}", nic.because("a reset that never cleared was not refused"));
    };
    assert!(
        after_nanos >= 100_000_000,
        "{}",
        nic.because("the deadline was called before it was reached")
    );
}

/// The claim answering neither a count nor "nothing since the last read" is the
/// kernel saying the function is no longer this driver's, and a pass that read
/// it as quiet would go on driving a device it no longer holds.
#[test]
fn a_claim_that_stops_answering_is_handed_up_and_not_read_as_quiet() {
    let nic = Nic::new(20);
    let mut driver = open(&nic);
    nic.set_link(true);
    one_pass(&mut driver);
    nic.claim_taken_away();
    assert_eq!(driver.begin_pass(), Err(Unanswered::Gone));
}

/// §10.2.4.1's `RXO` and `RXDMT0`: the two the part reports about its own
/// receive path, which are counted and never acted on.
#[test]
fn the_receiver_reporting_on_itself_is_counted() {
    let nic = Nic::with(21, Part::E82574, Permits { spurious_interrupts: false, ..Permits::default() });
    let mut driver = open(&nic);
    nic.set_link(true);
    one_pass(&mut driver);
    nic.cause(cause::RXO | cause::RXDMT0);
    one_pass(&mut driver);
    assert_eq!(driver.counters().overruns, 1, "{}", nic.because("RXO was not counted"));
    assert_eq!(driver.counters().starved, 1, "{}", nic.because("RXDMT0 was not counted"));
}

/// §7.2.10.1: one legacy descriptor carries one buffer. A frame longer than one
/// is refused and counted apart from a full ring, because it is a caller that
/// offered more than it was told it could.
#[test]
fn a_frame_longer_than_a_transmit_buffer_is_refused_and_not_truncated() {
    let nic = Nic::new(22);
    let mut driver = open(&nic);
    nic.set_link(true);
    assert!(driver.tx_reserve(TX_BUF_BYTES + 1).is_none());
    assert_eq!(driver.counters().too_long, 1);
    assert_eq!(
        driver.counters().tx_dropped,
        0,
        "{}",
        nic.because("a frame that never fit was counted as a full ring")
    );
    // And the ring is untouched: the next frame that does fit still goes out.
    let payload = frame(0x99, TX_BUF_BYTES);
    let slot = driver.tx_reserve(payload.len()).expect("a free transmit descriptor");
    nic.put_bytes(slot.at, &payload);
    driver.tx_commit(slot);
    nic.run();
    assert_eq!(nic.sent(), vec![payload]);
}

/// §7.4.5 names the spurious interrupt: a message whose cause is already gone.
/// It costs a pass and nothing else.
#[test]
fn a_spurious_interrupt_costs_a_pass_and_nothing_else() {
    // The model's own dice are off, so the one message below is the only one
    // and the count it is asserted against is exact.
    let nic = Nic::with(13, Part::E82574, Permits { spurious_interrupts: false, ..Permits::default() });
    let mut driver = open(&nic);
    nic.set_link(true);
    one_pass(&mut driver);
    // Every cause already clear, and a message all the same.
    nic.spurious();
    let pass = one_pass(&mut driver);
    assert_eq!(pass.messages, 1, "{}", nic.because("the message was not delivered"));
    assert_eq!(pass.causes & !cause::INT_ASSERTED, 0);
    assert_eq!(driver.counters().spurious, 1);
    assert_eq!(drain(&nic, &mut driver).len(), 0);
    // And the driver still works afterwards.
    let sent = frame(0x44, 200);
    nic.deliver(&sent);
    nic.run();
    one_pass(&mut driver);
    assert_eq!(drain(&nic, &mut driver), vec![sent]);
}

/// §10.2.4.1: `LSC` "is set whenever the link status changes (either from up to
/// down, or from down to up)", and §10.2.2.2's `LU` is what it means.
#[test]
fn the_link_going_away_and_coming_back_is_seen() {
    let nic = Nic::new(14);
    let mut driver = open(&nic);
    assert_eq!(
        driver.link(),
        Link::Down,
        "{}",
        nic.because("the link was up before anything plugged in")
    );

    nic.set_link(true);
    let up = one_pass(&mut driver);
    assert!(up.link_changed);
    assert_eq!(driver.link(), Link::Up { speed: Speed::Mbps1000, full_duplex: true });
    let at = driver.link_up_after_nanos().expect("a link-up time");

    nic.set_link(false);
    let down = one_pass(&mut driver);
    assert!(down.causes & cause::LSC != 0, "{}", nic.because("no LSC for the link going away"));
    assert!(down.link_changed);
    assert_eq!(driver.link(), Link::Down);

    nic.set_link(true);
    one_pass(&mut driver);
    assert!(driver.link().is_up());
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
    one_pass(&mut driver);
    assert_eq!(drain(&nic, &mut driver), vec![first]);

    nic.set_link(false);
    one_pass(&mut driver);
    nic.set_link(true);
    one_pass(&mut driver);

    let second = frame(2, 700);
    nic.deliver(&second);
    nic.run();
    one_pass(&mut driver);
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
            one_pass(&mut driver);
            got_in.extend(drain(&nic, &mut driver));
            got_out.extend(nic.sent());
        }
        for _ in 0..64 {
            nic.run();
            one_pass(&mut driver);
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
// --- the PHY behind MDIC ---

/// With a partner on the wire and the PHY where a part whose Management Engine
/// drives it leaves one — powered down, isolated and advertising what §6.1.5's
/// battery saver left — the bring-up has to satisfy every clause of §9 the stub
/// holds it to before the model raises a link at all.
#[test]
fn the_phy_is_brought_up_and_the_mac_sees_the_link_it_raises() {
    let nic = Nic::i219(31);
    // The wire before the driver, so the only thing between a partner and a
    // link is what `open` does to the PHY.
    nic.set_link(true);
    let mut driver = open(&nic);

    assert_eq!(
        driver.brought_up().phy.map(|phy| (phy.addr, phy.id)),
        Ok((toyos_phy::SPECIFIC, 0x0154_00a1)),
        "{}",
        nic.because("the PHY did not answer §9.5.2.3 as its own document says")
    );
    assert!(driver.brought_up().master_quiet, "{}", nic.because("§3.1.3.10 never went quiet"));
    // §4.5.2: "Once the access completes, the controlling agent must write a 0b
    // to its ownership bit to enable accesses by the other agents."
    assert_eq!(
        nic.peek(regs::EXTCNF_CTRL) & extcnf::MDIO_SW_OWNERSHIP,
        0,
        "{}",
        nic.because("the MDIO interface was left held after the bring-up")
    );
    assert_eq!(
        nic.phy_unconfigured(),
        Some("the restart of auto-negotiation has not resolved yet"),
        "{}",
        nic.because("the bring-up left a clause of §9 other than the negotiation outstanding")
    );

    nic.negotiation_settles();
    assert_eq!(
        nic.phy_unconfigured(),
        None,
        "{}",
        nic.because("the bring-up left the PHY unable to raise a link")
    );
    one_pass(&mut driver);
    assert_eq!(
        driver.link(),
        Link::Up { speed: Speed::Mbps1000, full_duplex: true },
        "{}",
        nic.because("§4.6.3.2's STATUS.LU did not follow the link the PHY raised")
    );
}

/// **What §9.5.2.5's advertisement is for, ability by ability.** The link
/// resolves to the best the two ends have in common, so every ability
/// [`toyos_phy::advertise::WANTED`] offers is the whole of what a partner
/// offering only that one can reach — and a partner offering nothing this part
/// advertises gets no link at all.
#[test]
fn the_advertised_abilities_are_what_the_link_resolves_to() {
    use toyos_phy::advertise::{FULL_10, FULL_100, HALF_10, HALF_100, SELECTOR_802_3};
    let only = |ability| SELECTOR_802_3 | ability;
    for (seed, partner, resolved) in [
        (42, only(HALF_10), Some((Speed::Mbps10, false))),
        (43, only(FULL_10), Some((Speed::Mbps10, true))),
        (44, only(HALF_100), Some((Speed::Mbps100, false))),
        (45, only(FULL_100), Some((Speed::Mbps100, true))),
        // §9.5.2.5's Selector Field alone: a partner with no ability at bits
        // 8:5 has nothing in common with this one, which is a cable that
        // carries no link.
        (46, SELECTOR_802_3, None),
    ] {
        let nic = Nic::i219(seed);
        // No 1000BASE-T on the partner's side, or §9.5.2.10's ability would
        // decide every row alike.
        nic.partner_advertises(partner, 0);
        nic.set_link(true);
        let mut driver = open(&nic);

        nic.negotiation_settles();
        one_pass(&mut driver);
        let wanted = match resolved {
            Some((speed, full_duplex)) => Link::Up { speed, full_duplex },
            None => Link::Down,
        };
        assert_eq!(
            driver.link(),
            wanted,
            "{}",
            nic.because(&format!(
                "against a partner advertising {partner:#06x} the link resolved to something \
                 other than the best ability the two have in common"
            ))
        );
    }
}

/// The premise of the test above: this model really does refuse a link to a PHY
/// nothing configured, so a green there is not a model that raises one for a
/// partner alone. The driver reaches no PHY register at all here, because
/// §4.5.2's arbitration never grants its request on this reading of the clause.
#[test]
fn a_partner_on_an_unconfigured_phy_raises_no_link() {
    let nic = Nic::with(
        32,
        Part::I219,
        Permits { mdio_flag_is_a_plain_mutex: false, ..Permits::default() },
    );
    nic.mdio_never_granted();
    nic.set_link(true);
    let driver = open(&nic);

    assert_eq!(
        nic.phy_unconfigured(),
        Some("§9.2's bit 10 of page 769 register 16 was never set"),
        "{}",
        nic.because("the model let a partner raise a link on a PHY nothing had touched")
    );
    assert!(
        !driver.link().is_up(),
        "{}",
        nic.because("STATUS.LU came up with the PHY unconfigured")
    );
}

/// **What the T14 answers, and the whole reason this driver asks at all.**
/// §4.5.2's manageability agent holds the interface and does not let go, and
/// the part grants the software request registered under it anyway — the clause
/// says "at any given time at most only one bit is 1b" and that silicon reads
/// back both. A driver that waited for the engine's bit to go would wait out
/// the boot on a part that was never going to take it away.
#[test]
fn a_part_that_grants_over_the_engines_standing_bit_is_brought_up() {
    let nic = Nic::i219(33);
    nic.mdio_never_granted();
    nic.set_link(true);
    let mut driver = open(&nic);

    assert_eq!(
        driver.brought_up().phy.map(|phy| (phy.addr, phy.id)),
        Ok((toyos_phy::SPECIFIC, 0x0154_00a1)),
        "{}",
        nic.because("the bring-up did not take an interface the part offered it")
    );
    // The premise: the engine's own bit stood for the whole of it, so the grant
    // this bring-up drove on was one it shared.
    assert!(
        nic.ownership_readings()
            .iter()
            .all(|reading| reading & extcnf::MDIO_MNG_OWNERSHIP != 0),
        "{}",
        nic.because("the engine let go, so this is not the reading the T14 gives")
    );
    assert_eq!(
        nic.peek(regs::EXTCNF_CTRL) & extcnf::OWNERSHIP,
        extcnf::MDIO_MNG_OWNERSHIP,
        "{}",
        nic.because("the request was left standing over the engine's own bit")
    );
    nic.negotiation_settles();
    one_pass(&mut driver);
    assert!(driver.link().is_up(), "{}", nic.because("STATUS.LU did not follow the PHY's link"));
}

/// The other reading of §4.5.2 — "access is not granted as long as the bit is
/// 0b" — on a part whose engine never lets go: the request is registered, the
/// bit never reads back set, and what is refused is the *grant* and not the
/// interface. **The judge is what the arbitration does once the engine lets
/// go**: a request still registered would be granted then, to a driver that
/// stopped waiting, so no grant may appear.
#[test]
fn a_grant_that_never_comes_is_refused_by_name_and_the_request_withdrawn() {
    let nic = Nic::with(
        63,
        Part::I219,
        Permits { mdio_flag_is_a_plain_mutex: false, ..Permits::default() },
    );
    nic.mdio_never_granted();
    let driver = open(&nic);

    match driver.brought_up().phy {
        Err(PhyRefusal::GrantNeverCame { held_by, after_nanos }) => {
            assert!(
                held_by & extcnf::MDIO_MNG_OWNERSHIP != 0,
                "{}",
                nic.because("the refusal does not carry the engine's bit")
            );
            assert!(
                after_nanos >= toyos_phy::ARBITRATION_DEADLINE_NANOS,
                "{}",
                nic.because("the grant was given up on before the deadline it is owed")
            );
        }
        other => panic!("{}", nic.because(&format!("the bring-up answered {other:?}"))),
    }

    nic.engine_lets_go();
    assert_eq!(
        nic.peek(regs::EXTCNF_CTRL) & extcnf::OWNERSHIP,
        0,
        "{}",
        nic.because(
            "the engine let go and the arbitration granted a request this driver had stopped \
             waiting on, so the request was never withdrawn"
        )
    );
}

/// A software bit already set when this driver looks is another agent's under
/// either reading of §4.5.2 — a grant it holds, or a flag it set — and this
/// driver's own request is that same bit, so nothing is registered over it and
/// nothing is cleared on the way out.
#[test]
fn a_flag_another_agent_holds_is_neither_asked_over_nor_cleared() {
    for (seed, mutex) in [(62, true), (65, false)] {
        let nic = Nic::with(
            seed,
            Part::I219,
            Permits { mdio_flag_is_a_plain_mutex: mutex, ..Permits::default() },
        );
        nic.mdio_flag_held_by_another_agent();
        nic.set_link(true);
        let driver = open(&nic);

        match driver.brought_up().phy {
            Err(PhyRefusal::SoftwareFlagStood { beside, after_nanos }) => {
                assert_eq!(
                    beside,
                    Others::in_reading(
                        nic.ownership_readings().last().copied().expect("a reading of the bits")
                    ),
                    "{}",
                    nic.because("the refusal does not carry who stood beside the flag")
                );
                assert!(
                    after_nanos >= toyos_phy::ARBITRATION_DEADLINE_NANOS,
                    "{}",
                    nic.because("the flag was given up on before the deadline it is owed")
                );
            }
            other => panic!("{}", nic.because(&format!("the bring-up answered {other:?}"))),
        }
        // Nothing at all was written to that register on this path, which is
        // the whole claim: no request over another agent's flag, and therefore
        // no release of one either.
        assert!(
            !nic.written().contains(&regs::EXTCNF_CTRL),
            "{}",
            nic.because("a request was registered over a flag that was already somebody's")
        );
        assert_eq!(
            nic.peek(regs::EXTCNF_CTRL) & extcnf::MDIO_SW_OWNERSHIP,
            extcnf::MDIO_SW_OWNERSHIP,
            "{}",
            nic.because("the bring-up cleared a semaphore another agent was holding")
        );
    }
}

/// §4.5.2 lets the manageability agent register its request at any moment, the
/// one between this driver reading the register and writing its own request
/// included; "the priority order is manageability, software and then hardware",
/// so the request registered under it is answered second and not lost.
#[test]
fn a_request_registered_while_the_engine_holds_it_is_still_granted() {
    for (seed, mutex) in [(67, true), (68, false)] {
        let nic = Nic::with(
            seed,
            Part::I219,
            Permits { mdio_flag_is_a_plain_mutex: mutex, ..Permits::default() },
        );
        nic.set_link(true);
        let mut driver = open(&nic);

        // The premise: the engine really did hold the interface at the moment
        // the request was registered.
        assert!(
            nic.ownership_readings()
                .first()
                .is_some_and(|reading| reading & extcnf::MDIO_MNG_OWNERSHIP != 0),
            "{}",
            nic.because("the engine was not on the interface when this driver asked")
        );
        assert_eq!(
            driver.brought_up().phy.map(|phy| (phy.addr, phy.id)),
            Ok((toyos_phy::SPECIFIC, 0x0154_00a1)),
            "{}",
            nic.because("the bring-up did not wait the engine out and reach the PHY")
        );
        nic.negotiation_settles();
        one_pass(&mut driver);
        assert!(
            driver.link().is_up(),
            "{}",
            nic.because("STATUS.LU did not follow the PHY's link")
        );
    }
}

/// **What [`toyos_phy::ARBITRATION_DEADLINE_NANOS`] is for.** A grant that comes
/// after several paces and still inside the bound is one this driver takes: the
/// engine holds the interface past the first readings of the request and lets
/// go inside the wait, and the bring-up goes on to the PHY. A narrower bound
/// would refuse a part that was answering.
#[test]
fn a_grant_that_comes_late_inside_the_bound_is_taken() {
    // Far enough past the request — registered about §9.2's 10 ms after the
    // reset — to be several paces late, and well inside the bound.
    let lets_go = 3 * toyos_phy::ARBITRATION_PACE_NANOS;
    assert!(lets_go + toyos_phy::LCD_RESET_DELAY_NANOS < toyos_phy::ARBITRATION_DEADLINE_NANOS);
    let nic = Nic::with(
        84,
        Part::I219,
        Permits { mdio_flag_is_a_plain_mutex: false, ..Permits::default() },
    );
    nic.engine_holds_the_interface_until(lets_go);
    nic.set_link(true);
    let mut driver = open(&nic);

    assert_eq!(
        driver.brought_up().phy.map(|phy| (phy.addr, phy.id)),
        Ok((toyos_phy::SPECIFIC, 0x0154_00a1)),
        "{}",
        nic.because("a grant that came late inside the bound was not taken")
    );
    // The premise: it really was late, and the bring-up really did wait.
    assert!(
        nic.arbitration_at().iter().filter(|at| **at < lets_go).count() >= 2,
        "{}",
        nic.because("the engine let go before this driver had to wait for it")
    );
    nic.negotiation_settles();
    one_pass(&mut driver);
    assert!(driver.link().is_up(), "{}", nic.because("STATUS.LU did not follow the PHY's link"));
}

/// **The pace, which is a bench fact and not a clause.** Two accesses to
/// §4.5.2's arbitration are never nearer each other than
/// [`toyos_phy::ARBITRATION_PACE_NANOS`]: on the T14 the same request put to
/// the same register about a millisecond apart left a machine that had to be
/// powered off, and the run that paced it came back and answered. **What this
/// test is about is that the pace is kept**; the floor under the number itself
/// is a `const` assertion beside it, so a narrower one does not compile.
#[test]
fn no_two_accesses_to_the_arbitration_are_nearer_than_the_pace() {
    for (seed, arrange) in [
        (80u64, (|_: &Nic| {}) as fn(&Nic)),
        (81, |n: &Nic| n.mdio_never_granted()),
        (82, |n: &Nic| n.mdio_flag_held_by_another_agent()),
        (83, |n: &Nic| n.mdi_never_ready()),
    ] {
        let nic = Nic::i219(seed);
        arrange(&nic);
        let _ = open(&nic);

        let at = nic.arbitration_at();
        assert!(at.len() >= 2, "{}", nic.because("this bring-up barely reached the arbitration"));
        for pair in at.windows(2) {
            assert!(
                pair[1] - pair[0] >= toyos_phy::ARBITRATION_PACE_NANOS,
                "{}",
                nic.because(&format!(
                    "two accesses to EXTCNF_CTRL are {} ns apart and the pace is {} ns",
                    pair[1] - pair[0],
                    toyos_phy::ARBITRATION_PACE_NANOS
                ))
            );
        }
    }
}

/// §4.5.2: "once the access completes, the controlling agent must write a 0b to
/// its ownership bit to enable accesses by the other agents" — on the path that
/// finished the bring-up and on every path that left it early alike. A bit left
/// standing keeps the Management Engine off the PHY they share for the boot.
#[test]
fn the_interface_is_given_back_on_every_path_that_took_it() {
    for (seed, arrange) in [
        (90u64, (|_: &Nic| {}) as fn(&Nic)),
        (91, |n: &Nic| n.mdi_never_ready()),
        (92, |n: &Nic| n.mdi_fails_read_of(toyos_phy::SPECIFIC, toyos_phy::reg::IDENTIFIER_HIGH)),
        (93, |n: &Nic| n.phy_identifies_as(0x1234)),
        (94, |n: &Nic| n.phy_is_deaf_at(toyos_phy::SPECIFIC)),
    ] {
        let nic = Nic::i219(seed);
        arrange(&nic);
        let driver = open(&nic);

        // The premise: the request really was registered on this path, so the
        // release below is a bit this driver had.
        assert!(
            nic.ownership_readings()
                .iter()
                .any(|reading| reading & extcnf::MDIO_SW_OWNERSHIP != 0),
            "{}",
            nic.because("no request was ever registered, so nothing was there to give back")
        );
        assert_eq!(
            nic.peek(regs::EXTCNF_CTRL) & extcnf::MDIO_SW_OWNERSHIP,
            0,
            "{}",
            nic.because(&format!(
                "the interface was left held after the bring-up answered {:?}",
                driver.brought_up().phy
            ))
        );
    }
}

/// The link the probe waits for, on the three answers a boot can carry off a
/// machine with no console: a PHY brought up onto a link, a PHY brought up onto
/// a cable with nothing at the other end, and a part whose PHY this bring-up
/// refuses — whose link is not this driver's to wait for.
#[test]
fn the_probe_waits_a_bounded_time_for_the_link_and_names_what_came() {
    use toyos_phy::Outcome;

    let raised = Nic::i219(95);
    raised.set_link(true);
    let mut driver = open(&raised);
    raised.negotiation_settles();
    assert_eq!(
        driver.probe_outcome(),
        Outcome::LinkAt1000Full,
        "{}",
        raised.because("the probe did not name the link the PHY raised")
    );

    let quiet = Nic::i219(96);
    let mut driver = open(&quiet);
    let started = quiet.now();
    assert_eq!(
        driver.probe_outcome(),
        Outcome::BroughtUpNoLink,
        "{}",
        quiet.because("the probe named a link on a cable with nothing at the other end")
    );
    assert!(
        quiet.now() - started >= LINK_DEADLINE_NANOS,
        "{}",
        quiet.because("the link was given up on before the bound it is owed")
    );

    let refused = Nic::new(97);
    let mut driver = open(&refused);
    let started = refused.now();
    assert_eq!(
        driver.probe_outcome(),
        Outcome::NotThisRegisterMap,
        "{}",
        refused.because("the probe did not carry the bring-up's own refusal")
    );
    assert!(
        refused.now() - started < LINK_DEADLINE_NANOS,
        "{}",
        refused.because("a bring-up that refused the PHY waited for its link anyway")
    );
}

/// A window that answers ones at one offset decodes nothing there, and what it
/// answered is not a register's value to carry: §4.5.2 gives this driver one
/// bit of `EXTCNF_CTRL`, and a read-modify-write over ones would set every
/// other field of it — including the fields that point the part's configuration
/// engine somewhere. The model asserts on the write, so this test passing is
/// the write not being made.
#[test]
fn a_register_nothing_decodes_is_refused_and_never_written() {
    let nic = Nic::i219(64);
    nic.window_does_not_decode(regs::EXTCNF_CTRL);
    let driver = open(&nic);

    assert_eq!(
        driver.brought_up().phy,
        Err(PhyRefusal::Unrouted { reg: regs::EXTCNF_CTRL }),
        "{}",
        nic.because("a register answering ones was taken for one this driver may write")
    );
}

/// Every answer the probe can give has one exit code, and every code reads back
/// as its answer: the table netd exits through and the harness decodes with is
/// one declaration, so the two ends cannot disagree about a number.
#[test]
fn every_probe_outcome_has_one_exit_code_that_reads_back() {
    use toyos_phy::Outcome;
    let up = Ok(Phy { addr: toyos_phy::SPECIFIC, id: 0x0154_00a1, port_general: 0 });
    let link = |speed, full_duplex| Link::Up { speed, full_duplex };
    let stood = |beside| Err(PhyRefusal::SoftwareFlagStood { beside, after_nanos: 1 });
    let down = Link::Down;
    let outcomes: [(Result<Phy, PhyRefusal>, Link, Outcome); 19] = [
        (up, down, Outcome::BroughtUpNoLink),
        (up, link(Speed::Mbps10, false), Outcome::LinkAt10Half),
        (up, link(Speed::Mbps10, true), Outcome::LinkAt10Full),
        (up, link(Speed::Mbps100, false), Outcome::LinkAt100Half),
        (up, link(Speed::Mbps100, true), Outcome::LinkAt100Full),
        (up, link(Speed::Mbps1000, false), Outcome::LinkAt1000Half),
        (up, link(Speed::Mbps1000, true), Outcome::LinkAt1000Full),
        (Err(PhyRefusal::Unrouted { reg: regs::EXTCNF_CTRL }), down, Outcome::Unrouted),
        (stood(Others::Nobody), down, Outcome::SoftwareFlagStood),
        (stood(Others::Hardware), down, Outcome::SoftwareFlagStoodBesideHardware),
        (stood(Others::Manageability), down, Outcome::SoftwareFlagStoodBesideManageability),
        (stood(Others::HardwareAndManageability), down, Outcome::SoftwareFlagStoodBesideBoth),
        (
            Err(PhyRefusal::GrantNeverCame { held_by: 0x80, after_nanos: 1 }),
            down,
            Outcome::GrantNeverCame,
        ),
        (Err(PhyRefusal::MdiUnready { phy: 1, reg: 31, after_nanos: 1 }), down, Outcome::MdiUnready),
        (Err(PhyRefusal::MdiError { phy: 2, reg: 2 }), down, Outcome::MdiError),
        (Err(PhyRefusal::Identity { specific: 0, general: 0 }), down, Outcome::Identity),
        (Err(PhyRefusal::NotThisRegisterMap), down, Outcome::NotThisRegisterMap),
        (
            Err(PhyRefusal::MdiWaiting { phy: 1, reg: 2, after_nanos: 3 }),
            down,
            Outcome::MdiWaiting,
        ),
        (
            Err(PhyRefusal::PhyPoweredDown { ctrl: 1 << 24, ctrl_ext: 0 }),
            down,
            Outcome::PhyPoweredDown,
        ),
    ];
    // The block's base is pinned, not read off the table it is judging.
    assert_eq!(Outcome::ALL[0].exit_code(), 64, "the block no longer starts at 64");
    let mut codes = Vec::new();
    for (phy, link, outcome) in outcomes {
        assert_eq!(Outcome::of(phy, link), outcome, "{phy:?} {link:?}");
        let code = outcome.exit_code();
        assert!((64..128).contains(&code), "{outcome:?} exits {code}");
        assert_ne!(code, 101, "{outcome:?} exits the code a panicking netd ends with");
        assert_eq!(Outcome::from_exit_code(code), Some(outcome));
        assert!(!codes.contains(&code), "{outcome:?} shares {code} with another outcome");
        codes.push(code);
    }
    assert_eq!(codes.len(), Outcome::ALL.len());
    assert_eq!(Outcome::from_exit_code(0), None);
    assert_eq!(Outcome::from_exit_code(Outcome::ALL.len() as i32 + 64), None);

    // A refusal carries no link, whatever the part's `STATUS` was saying: the
    // agent before this driver may have left one up.
    for (phy, _, outcome) in outcomes.into_iter().filter(|(phy, ..)| phy.is_err()) {
        assert_eq!(Outcome::of(phy, link(Speed::Mbps1000, true)), outcome, "{phy:?}");
    }
}

/// §4.5.2 arbitrates three bits of `EXTCNF_CTRL` and no more, so who stands
/// beside another agent's software flag is decided by the other two and by
/// nothing else in the word.
#[test]
fn every_reading_of_the_other_two_ownership_bits_names_who_stands_beside_the_flag() {
    let elsewhere = !extcnf::OWNERSHIP;
    for (bits, others) in [
        (0, Others::Nobody),
        (extcnf::MDIO_HW_OWNERSHIP, Others::Hardware),
        (extcnf::MDIO_MNG_OWNERSHIP, Others::Manageability),
        (
            extcnf::MDIO_HW_OWNERSHIP | extcnf::MDIO_MNG_OWNERSHIP,
            Others::HardwareAndManageability,
        ),
    ] {
        for flag in [0, extcnf::MDIO_SW_OWNERSHIP] {
            assert_eq!(
                Others::in_reading(bits | flag),
                others,
                "{bits:#010x} names somebody else"
            );
            assert_eq!(
                Others::in_reading(bits | flag | elsewhere),
                others,
                "another agent's fields of EXTCNF_CTRL decided who stands beside {bits:#010x}"
            );
        }
    }
}

/// **The one question a probe boot on a machine with no console can answer.**
/// Another software agent's flag is the refusal whose cause is another agent,
/// so each reading of the two bits that can stand beside it is driven on the
/// part and the exit code it produces is asserted: one code each, all four
/// distinct.
#[test]
fn each_agent_standing_beside_the_flag_has_its_own_exit_code() {
    use toyos_phy::Outcome;
    let mut codes = Vec::new();
    for (seed, hardware, manageability, wanted) in [
        (70, false, false, Outcome::SoftwareFlagStood),
        (71, true, false, Outcome::SoftwareFlagStoodBesideHardware),
        (72, false, true, Outcome::SoftwareFlagStoodBesideManageability),
        (73, true, true, Outcome::SoftwareFlagStoodBesideBoth),
    ] {
        let nic = Nic::i219(seed);
        nic.mdio_flag_held_by_another_agent();
        if hardware {
            nic.hardware_holds_the_mdio_interface();
        }
        if manageability {
            nic.mdio_never_granted();
        }
        let driver = open(&nic);

        let phy = driver.brought_up().phy;
        assert!(
            matches!(phy, Err(PhyRefusal::SoftwareFlagStood { .. })),
            "{}",
            nic.because(&format!("the bring-up answered {phy:?} on a flag another agent holds"))
        );
        // The part really did answer the bits this row is about, so the code
        // below is about that reading and not about a model that lost one.
        assert_eq!(
            nic.peek(regs::EXTCNF_CTRL) & extcnf::OWNERSHIP,
            extcnf::MDIO_SW_OWNERSHIP
                | if hardware { extcnf::MDIO_HW_OWNERSHIP } else { 0 }
                | if manageability { extcnf::MDIO_MNG_OWNERSHIP } else { 0 },
            "{}",
            nic.because("the part did not stand the agents this row names")
        );
        let outcome = Outcome::of(phy, Link::Down);
        assert_eq!(
            outcome,
            wanted,
            "{}",
            nic.because("the exit code does not name which agent stood beside the flag")
        );
        let code = outcome.exit_code();
        assert!(
            !codes.contains(&code),
            "{}",
            nic.because(&format!("{outcome:?} shares exit code {code} with another reading"))
        );
        codes.push(code);
    }
    assert_eq!(codes.len(), 4);
}

/// §4.5.2's arbitration moves while a driver waits on it, and what a probe
/// carries off a machine is the interface at the moment the wait ended — so the
/// refusal names the *last* reading and not the one it started on.
#[test]
fn the_refusal_names_the_last_reading_before_the_deadline() {
    let nic = Nic::i219(77);
    // Another agent's flag stands for the whole wait; the part's own hardware
    // holds the interface too, and the manageability agent holds it across the
    // reads §4.5.2 gives it to load the extended configuration area and then
    // lets go.
    nic.mdio_flag_held_by_another_agent();
    nic.hardware_holds_the_mdio_interface();
    let driver = open(&nic);

    // The premise: the reading really did move under the wait, and the two it
    // moved between are two different codes.
    let readings = nic.ownership_readings();
    assert_eq!(
        readings.first().copied(),
        Some(extcnf::OWNERSHIP),
        "{}",
        nic.because(&format!("the wait did not start on all three agents: {readings:#x?}"))
    );
    assert_eq!(
        readings.last().copied(),
        Some(extcnf::MDIO_SW_OWNERSHIP | extcnf::MDIO_HW_OWNERSHIP),
        "{}",
        nic.because(&format!("the wait did not end on two agents: {readings:#x?}"))
    );

    match driver.brought_up().phy {
        Err(PhyRefusal::SoftwareFlagStood { beside, .. }) => assert_eq!(
            beside,
            Others::Hardware,
            "{}",
            nic.because("the refusal named the reading the wait began on")
        ),
        other => panic!("{}", nic.because(&format!("the bring-up answered {other:?}"))),
    }
}

/// §4.5.2 arbitrates three bits of `EXTCNF_CTRL` and this driver writes one of
/// them, so what stands in the rest of that register outlives the bring-up — on
/// the path that is granted the interface and on the one that withdraws its
/// request alike.
#[test]
fn the_ownership_claim_leaves_the_rest_of_the_register_standing() {
    let elsewhere = |nic: &Nic| nic.peek(regs::EXTCNF_CTRL) & !extcnf::OWNERSHIP;

    let granted = Nic::i219(51);
    granted.set_link(true);
    let before = elsewhere(&granted);
    assert_ne!(
        before,
        0,
        "{}",
        granted.because("the part came up with nothing outside §4.5.2's three bits, so a \
                         composed write would have had nothing to clear")
    );
    let mut driver = open(&granted);
    assert!(driver.brought_up().phy.is_ok(), "{}", granted.because("the PHY was not reached"));
    granted.negotiation_settles();
    one_pass(&mut driver);
    assert_eq!(
        elsewhere(&granted),
        before,
        "{}",
        granted.because("the bring-up cleared fields of EXTCNF_CTRL that are not its own")
    );

    // The withdrawal on the deadline is the other write of that register, and
    // it is made having been granted nothing. It runs only where a request was
    // registered at all — which is every path but the one that found another
    // software agent's flag standing.
    let refused = Nic::with(
        52,
        Part::I219,
        Permits { mdio_flag_is_a_plain_mutex: false, ..Permits::default() },
    );
    refused.mdio_never_granted();
    let before = elsewhere(&refused);
    let driver = open(&refused);
    assert!(
        matches!(driver.brought_up().phy, Err(PhyRefusal::GrantNeverCame { .. })),
        "{}",
        refused.because("no request was registered, so the withdrawal never ran")
    );
    assert_eq!(
        elsewhere(&refused),
        before,
        "{}",
        refused.because("withdrawing the request cleared fields of EXTCNF_CTRL it does not own")
    );
}

/// §10.2.2.7's `Ready` is what says a transaction happened. A part that never
/// sets it is refused after a bounded wait, and the interface goes back.
#[test]
fn an_mdi_transaction_that_never_reports_ready_is_refused_by_name() {
    let nic = Nic::i219(34);
    nic.mdi_never_ready();
    let driver = open(&nic);

    match driver.brought_up().phy {
        Err(PhyRefusal::MdiUnready { phy, reg, after_nanos }) => {
            // The first transaction the bring-up makes is §9.3's page select.
            assert_eq!((phy, reg), (toyos_phy::GENERAL, toyos_phy::reg::PAGE_SELECT));
            assert!(
                after_nanos >= toyos_phy::MDI_DEADLINE_NANOS,
                "{}",
                nic.because("the transaction was given up on before the deadline it is owed")
            );
        }
        other => panic!("{}", nic.because(&format!("the bring-up answered {other:?}"))),
    }
    assert_eq!(
        nic.peek(regs::EXTCNF_CTRL) & extcnf::OWNERSHIP,
        0,
        "{}",
        nic.because("the MDIO interface was left held after a transaction that never finished")
    );
}

/// §10.2.2.7's `Error` is set "when it fails to complete an MDI read", and
/// `Ready` comes back with it because the transaction ended — so a driver that
/// read `Ready` first would believe a data field the PHY never drove.
#[test]
fn an_mdi_read_the_part_could_not_complete_is_refused_by_name() {
    let nic = Nic::i219(35);
    nic.mdi_fails_read_of(toyos_phy::SPECIFIC, toyos_phy::reg::IDENTIFIER_HIGH);
    let driver = open(&nic);

    assert_eq!(
        driver.brought_up().phy,
        Err(PhyRefusal::MdiError {
            phy: toyos_phy::SPECIFIC,
            reg: toyos_phy::reg::IDENTIFIER_HIGH
        }),
        "{}",
        nic.because("a read the part flagged as failed was believed")
    );
}

/// §9.5.2.1's other two states the agent before this one can leave behind, each
/// named by the clause that holds the link down while it stands. The driver is
/// stopped at §9.5.2.3's identifier, so §9.2's bit is set and the Control
/// register is still the one it inherited.
#[test]
fn a_phy_left_in_loopback_or_configured_by_hand_raises_no_link() {
    // A PHY in reach, because a power cycle is a power-on reset (I219 Table
    // 5-1) and would take the inherited state this test is about with it.
    let left = |loopback, autonegotiation_disabled| Permits {
        phy_starts_out_of_reach: false,
        phy_starts_powered_down: false,
        phy_starts_in_loopback_or_resetting: loopback,
        phy_starts_with_autonegotiation_disabled: autonegotiation_disabled,
        ..Permits::default()
    };
    for (seed, permits, unconfigured) in [
        (48, left(true, false), "§9.5.2.1's Loopback was left set"),
        (49, left(false, true), "§9.5.2.1's Auto-Negotiation Enable was left clear"),
    ] {
        let nic = Nic::with(seed, Part::I219, permits);
        nic.set_link(true);
        nic.mdi_fails_read_of(toyos_phy::SPECIFIC, toyos_phy::reg::IDENTIFIER_HIGH);
        let driver = open(&nic);

        assert_eq!(
            nic.phy_unconfigured(),
            Some(unconfigured),
            "{}",
            nic.because("the model named a clause other than the one §9.5.2.1 left standing")
        );
        assert!(
            !driver.link().is_up(),
            "{}",
            nic.because("STATUS.LU came up with §9.5.2.1 outstanding")
        );
    }
}

/// §9.5.2.1: "Writing a 1b to this bit causes immediate PHY reset", and what
/// the register file comes back as is §9.5's own defaults — not the state the
/// agent before this driver left, which is what a claim inherits and what a
/// reset is the end of.
#[test]
fn a_phy_reset_restores_the_defaults_and_not_the_state_the_claim_inherited() {
    let nic = Nic::i219(50);
    nic.set_link(true);
    let mut driver = open(&nic);
    nic.negotiation_settles();
    one_pass(&mut driver);
    assert!(driver.link().is_up(), "{}", nic.because("the bring-up raised no link to take away"));

    nic.phy_is_reset();

    assert_eq!(
        nic.phy_unconfigured(),
        Some("§9.2's bit 10 of page 769 register 16 was never set"),
        "{}",
        nic.because("the reset left the driver's §9 sequence standing")
    );
    // §9.5.2.1's table: Speed Selection (MSB, bit 6), Duplex Mode (bit 8) and
    // Auto-Negotiation Enable (bit 12) come up 1b, and Power Down, Isolate,
    // Loopback and Reset itself are not among them.
    assert_eq!(
        nic.phy_peek(toyos_phy::SPECIFIC, toyos_phy::reg::CONTROL),
        (1 << 6) | (1 << 8) | (1 << 12),
        "{}",
        nic.because("§9.5.2.1's register 0 did not come back to its own default")
    );
    // §9.5.2.5's whole default is 0x01E1 — neither the 0x0061 §6.1.5's battery
    // saver leaves behind nor the advertisement this driver wrote over it.
    assert_eq!(
        nic.phy_peek(toyos_phy::SPECIFIC, toyos_phy::reg::ADVERTISE),
        0x01E1,
        "{}",
        nic.because("§9.5.2.5's advertisement did not come back to its own default")
    );
}

/// §9.3 and Table 9-1 place §9.5.2's registers at different PHY addresses, so
/// which one a part answers at is asked and not assumed: with nothing driving
/// the address the table names, the identifier is what finds the other one and
/// the link still comes up.
#[test]
fn the_phy_is_found_at_whichever_address_answers_its_identifier() {
    let nic = Nic::i219(41);
    nic.phy_is_deaf_at(toyos_phy::SPECIFIC);
    nic.set_link(true);
    let mut driver = open(&nic);

    assert_eq!(
        driver.brought_up().phy.map(|phy| (phy.addr, phy.id)),
        Ok((toyos_phy::GENERAL, 0x0154_00a1)),
        "{}",
        nic.because("the bring-up did not look past the address Table 9-1 names")
    );
    nic.negotiation_settles();
    one_pass(&mut driver);
    assert!(driver.link().is_up(), "{}", nic.because("STATUS.LU did not follow the PHY's link"));
}

/// §9.5.2.3's identifier is the one word in the PHY that says a transaction
/// reached it: a window that answers ones, an address nothing drives and a part
/// that is not this one all fail it, and none of them can forge it.
#[test]
fn a_phy_that_is_not_the_one_this_map_describes_is_refused_by_its_identifier() {
    // In reach, so no power cycle takes the inherited Power Down this test
    // reads as the witness that nothing was configured.
    let nic = Nic::with(
        36,
        Part::I219,
        Permits { phy_starts_out_of_reach: false, ..Permits::default() },
    );
    // What QEMU's `e1000e` answers for its own modelled PHY.
    nic.phy_identifies_as(0x0141);
    let driver = open(&nic);

    assert_eq!(
        driver.brought_up().phy,
        Err(PhyRefusal::Identity { specific: 0x0141_00a1, general: 0x0141_00a1 }),
        "{}",
        nic.because("a PHY whose identifier is not Intel's was configured anyway")
    );
    assert_eq!(
        nic.phy_unconfigured(),
        Some("§9.5.2.1's Power Down was left set"),
        "{}",
        nic.because("the bring-up configured a PHY it had already refused")
    );
}

/// §10.2.2.7 addresses the 82574's own PHY as "1 = Gigabit PHY. 2 = PCIe PHY"
/// and it has neither §9.3's page register nor §9.5.3's paged registers, so the
/// I219's sequence is refused on it by name. The stub asserts on any access to
/// `MDIC` there, which is what keeps the QEMU arm's part out of the path above.
#[test]
fn the_82574s_own_phy_register_map_is_refused_by_name() {
    let nic = Nic::new(37);
    let driver = open(&nic);
    assert_eq!(driver.brought_up().phy, Err(PhyRefusal::NotThisRegisterMap));
}

/// §3.1.3.10 says a driver "might time out if the PCIe Master Enable Status bit
/// is not cleared within a given time" and says nothing else, so the expiry is
/// recorded and the reset is issued: a card refused for it is a machine with no
/// network on a handshake the document itself calls optional.
#[test]
fn a_master_that_never_goes_quiet_does_not_stop_the_bring_up() {
    let nic = Nic::i219(38);
    nic.master_never_quiesces();
    nic.set_link(true);
    let mut driver = open(&nic);

    assert!(
        !driver.brought_up().master_quiet,
        "{}",
        nic.because("the quiesce was reported finished on a part that never finished it")
    );
    nic.negotiation_settles();
    one_pass(&mut driver);
    assert!(driver.link().is_up(), "{}", nic.because("the bring-up did not go on to a link"));
}

/// **Every latitude §9, §4.5.2, §10.2.2.7 and §3.1.3.10 give the hardware,
/// taken the other way.** A part that grants the MDIO interface on the first
/// read, reports `Ready` on the first read, comes up out of power-down and
/// still advertises §9.5.2.5's own default is as admissible as the one every
/// other test here runs against, and the same bring-up has to work on it.
#[test]
fn a_part_that_takes_none_of_the_datasheets_latitudes_is_brought_up_the_same_way() {
    let nic = Nic::with(
        47,
        Part::I219,
        Permits {
            master_takes_time_to_quiesce: false,
            firmware_takes_the_mdio_interface: false,
            mdi_takes_several_reads: false,
            phy_starts_powered_down: false,
            phy_starts_in_loopback_or_resetting: false,
            phy_starts_with_autonegotiation_disabled: false,
            phy_advertises_what_the_last_agent_left: false,
            extcnf_carries_firmware_fields: false,
            firmware_requests_after_a_free_read: false,
            ..Permits::default()
        },
    );
    nic.set_link(true);
    let mut driver = open(&nic);

    assert_eq!(
        driver.brought_up().phy.map(|phy| (phy.addr, phy.id)),
        Ok((toyos_phy::SPECIFIC, 0x0154_00a1)),
        "{}",
        nic.because("the bring-up did not reach the PHY on a part that made it easy")
    );
    assert_eq!(nic.engine_cut_ins(), 0, "{}", nic.because("the engine cut in with the latitude off"));
    assert!(driver.brought_up().master_quiet, "{}", nic.because("§3.1.3.10 never went quiet"));
    nic.negotiation_settles();
    one_pass(&mut driver);
    assert_eq!(
        driver.link(),
        Link::Up { speed: Speed::Mbps1000, full_duplex: true },
        "{}",
        nic.because("§4.6.3.2's STATUS.LU did not follow the link the PHY raised")
    );
}

use crate::crumbs::{self, before, Broken, Crumbed, Ending, Runs, Step, Trail};
use std::cell::RefCell;
use std::rc::Rc;
use std::string::{String, ToString};

/// One thing a crumbed bring-up did, in the order it did it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Seen {
    Crumb(Step),
    Reached(Step),
}

type Seens = Rc<RefCell<Vec<Seen>>>;

/// The part's own side of the order: what reached it, noted as it arrived.
struct Tape<R> {
    regs: R,
    seen: Seens,
}

impl<R: Registers> Registers for Tape<R> {
    fn bytes(&self) -> usize {
        self.regs.bytes()
    }

    fn read(&self, reg: usize) -> u32 {
        self.seen.borrow_mut().push(Seen::Reached(Step::Read { reg }));
        self.regs.read(reg)
    }

    fn write(&self, reg: usize, value: u32) {
        self.seen.borrow_mut().push(Seen::Reached(Step::Write { reg, value }));
        self.regs.write(reg, value);
    }
}

/// The trail's side of it.
struct Noted(Seens);

impl Trail for Noted {
    fn crumb(&self, step: Step) {
        self.0.borrow_mut().push(Seen::Crumb(step));
    }
}

/// The three parts a trail is owed on: the 82574, an I219 whose PHY comes up,
/// and an I219 that never grants the request this driver registers — the one
/// arm whose bring-up ends in §4.5.2 and not at the PHY.
fn crumbed_parts() -> [Nic; 3] {
    let ungranted = Nic::with(
        83,
        Part::I219,
        Permits { mdio_flag_is_a_plain_mutex: false, ..Permits::default() },
    );
    ungranted.engine_holds_the_interface_until(u64::MAX);
    [Nic::new(81), Nic::i219(82), ungranted]
}

/// A bring-up over a taped part, and everything the tape and the trail saw.
fn taped(nic: &Nic, trail: impl FnOnce(Seens) -> Option<Runs<Noted>>, every: bool) -> Vec<Seen> {
    let seen: Seens = Rc::default();
    let (bar, clock, grant, line) = nic.parts();
    let tape = Tape { regs: bar, seen: Rc::clone(&seen) };
    let opened = match (trail(Rc::clone(&seen)), every) {
        (Some(runs), _) => {
            I219::open(nic.part(), Crumbed::over(tape, runs), clock, grant, line).map(|_| ())
        }
        (None, true) => {
            I219::open(nic.part(), Crumbed::over(tape, Noted(Rc::clone(&seen))), clock, grant, line)
                .map(|_| ())
        }
        (None, false) => I219::open(nic.part(), tape, clock, grant, line).map(|_| ()),
    };
    opened.unwrap_or_else(|why| panic!("{}", nic.because(&format!("open refused it: {why}"))));
    let seen = seen.borrow().clone();
    seen
}

fn reached(seen: &[Seen]) -> Vec<Step> {
    seen.iter().filter_map(|s| if let Seen::Reached(step) = s { Some(*step) } else { None }).collect()
}

fn crumbs_of(seen: &[Seen]) -> Vec<Step> {
    seen.iter().filter_map(|s| if let Seen::Crumb(step) = s { Some(*step) } else { None }).collect()
}

/// The instrument's one claim: a crumb is on the trail before the access it
/// names reaches the part, for every access of a whole bring-up.
#[test]
fn every_crumb_is_left_before_its_access_reaches_the_part() {
    for nic in crumbed_parts() {
        let seen = taped(&nic, |_| None, true);
        assert!(seen.len() > 60, "{}", nic.because("the bring-up reached almost nothing"));
        for pair in seen.chunks(2) {
            match pair {
                [Seen::Crumb(crumb), Seen::Reached(access)] if crumb == access => {}
                other => panic!(
                    "{}",
                    nic.because(&format!("an access and its crumb arrived as {other:?}"))
                ),
            }
        }
    }
    // And the same order for a step that is not a register access.
    let seen: Seens = Rc::default();
    let took = before(&Noted(Rc::clone(&seen)), Step::MapBar, || {
        seen.borrow_mut().push(Seen::Reached(Step::MapBar));
        7
    });
    assert_eq!(took, 7);
    assert_eq!(*seen.borrow(), [Seen::Crumb(Step::MapBar), Seen::Reached(Step::MapBar)]);
}

/// A crumbed bring-up is the bring-up: the part is reached by the same accesses
/// carrying the same values in the same order, whether or not a trail is kept.
#[test]
fn a_crumbed_bring_up_reaches_the_part_exactly_as_a_bare_one_does() {
    for (bare, crumbed) in crumbed_parts().into_iter().zip(crumbed_parts()) {
        let without = reached(&taped(&bare, |_| None, false));
        let with = reached(&taped(&crumbed, |seen| Some(Runs::over(Noted(seen))), false));
        assert!(without == with, "{}", bare.because("the trail changed what reached the part"));
        assert_eq!(bare.now(), crumbed.now(), "{}", bare.because("the trail moved the clock"));
    }
}

/// A poll is one crumb however long it spins, and §4.5.2's arbitration is never
/// a poll: each access to it is a crumb of its own.
#[test]
fn a_run_is_one_crumb_and_the_arbitration_is_never_a_run() {
    let [_, _, ungranted] = crumbed_parts();
    ungranted.master_never_quiesces();
    let seen = taped(&ungranted, |seen| Some(Runs::over(Noted(seen))), false);
    let (crumbs, accesses) = (crumbs_of(&seen), reached(&seen));
    let of = |steps: &[Step], want: Step| steps.iter().filter(|s| **s == want).count();

    // Two of them at the very least — the reading before the request and one
    // after it — and every one on the trail under its own line. The count is
    // small because each is paced and never because a run was folded.
    let arbitration = Step::Read { reg: regs::EXTCNF_CTRL };
    assert!(of(&accesses, arbitration) > 1, "the engine never made this driver wait");
    assert_eq!(of(&crumbs, arbitration), of(&accesses, arbitration));

    let status = Step::Read { reg: regs::STATUS };
    assert!(of(&accesses, status) > 1000, "the master quiesce never made this driver wait");
    // The first read, the quiesce poll, the full reset's wait on
    // LAN_INIT_DONE and the link's.
    assert_eq!(of(&crumbs, status), 4);

    let table = |s: &&Step| matches!(s, Step::Write { reg, .. } if (regs::MTA..regs::RAL0).contains(reg));
    assert_eq!(accesses.iter().filter(table).count(), regs::MTA_DWORDS);
    assert_eq!(crumbs.iter().filter(table).count(), 1);
}


#[test]
fn the_82574s_trail_is_the_bring_up_in_the_datasheets_order() {
    let [plain, brought_up, held] = crumbed_parts();
    let trail = |nic: &Nic| -> Vec<String> {
        crumbs_of(&taped(nic, |seen| Some(Runs::over(Noted(seen))), false))
            .iter()
            .map(|step| step.named().to_string())
            .collect()
    };
    let owed: Vec<String> = crumbs::BRING_UP.iter().map(|s| s.to_string()).collect();
    assert_eq!(trail(&plain), owed);
    // The I219's is the same trail with the PHY's accesses and the PCH's own
    // bits in it and nothing else moved.
    // §8.2's power and handshake sweep is the PHY's step as much as `MDIC` is.
    // Four of its five registers are the PHY's alone and drop out; `CTRL` is
    // shared with the bring-up either side of it, so an access to that one is
    // allowed to be extra and everything else has to line up exactly.
    assert!(trail(&plain).iter().all(|step| !crumbs::is_the_pchs(step)));
    for nic in [brought_up, held] {
        let whole = trail(&nic);
        for reg in ["TXDCTL1", "TARC0", "TARC1", "RFCTL", "PBECCSTS", "WUC", "GCR", "FFLT_DBG"] {
            assert!(
                whole.iter().any(|step| step == &format!("write {reg}")),
                "{}",
                nic.because(&format!("the PCH's MAC never had {reg} written"))
            );
        }
        let mut owed = owed.iter();
        let mut next = owed.next();
        for step in whole
            .into_iter()
            .filter(|s| !crumbs::is_the_phys(s) && !crumbs::is_the_pchs(s))
        {
            if next == Some(&step) {
                next = owed.next();
                continue;
            }
            assert!(
                crumbs::is_shared_with_the_phys(&step),
                "{}",
                nic.because(&format!("the PHY moved the rest of the trail at {step:?}"))
            );
        }
        assert_eq!(next, None, "{}", nic.because("the bring-up lost a step"));
    }
}

#[test]
fn a_line_reads_back_as_it_was_written() {
    let steps = [
        Step::Start,
        Step::ClaimHeld,
        Step::Describe,
        Step::MapBar,
        Step::DmaAlloc,
        Step::Read { reg: regs::EXTCNF_CTRL },
        Step::Read { reg: 0x5b58 },
        Step::Write { reg: regs::CTRL, value: ctrl::RST | 0x40 },
        Step::Write { reg: regs::MTA, value: 0 },
        Step::Opened,
        Step::Exit { code: 69 },
    ];
    let mut file = String::new();
    for (seq, step) in steps.into_iter().enumerate() {
        let line = crumbs::Line { seq: seq as u32, at: 1_000 * seq as u64 + 7, synced: 990 * seq as u64, step };
        file.push_str(&format!("{line}\n"));
    }
    let read: Vec<Step> = crumbs::lines(&file).map(|l| l.expect("a line it wrote").step).collect();
    assert_eq!(read, steps);
    assert!(file.contains("\n007 7007 6930 write CTRL 0x04000040\n"), "{file}");
    assert!(file.contains(" read 0x05b58\n"), "{file}");
}

#[test]
fn where_a_trail_stops_is_what_it_says() {
    assert_eq!(Ending::of(""), Ok(Ending::Nothing));
    let two = "000 10 0 start\n001 50 40 claim-held\n";
    let Ok(Ending::In(last)) = Ending::of(two) else { panic!("{:?}", Ending::of(two)) };
    assert_eq!((last.seq, last.step, last.at, last.synced), (1, Step::ClaimHeld, 50, 40));
    assert!(Ending::In(last).to_string().contains("1 `claim-held` at 50 ns"));

    // The write the machine ended in carries no newline, and the crumb before
    // it is still the last one that was durable.
    for torn in ["002 90 80 map", "002 90 80 map-bar", "0"] {
        assert_eq!(Ending::of(&format!("{two}{torn}")), Ok(Ending::In(last)), "{torn:?}");
    }
    assert_eq!(Ending::of("000 10 0 sta"), Ok(Ending::Nothing));

    let whole = format!("{two}002 90 80 exit 69\n");
    let Ok(Ending::Complete { code: 69, last }) = Ending::of(&whole) else {
        panic!("{:?}", Ending::of(&whole))
    };
    assert!(Ending::Complete { code: 69, last }.to_string().contains("all 3 crumbs"));

    assert_eq!(
        Ending::of("000 10 0 start\n002 90 80 map-bar\n").map_err(|why| matches!(why, Broken::Gap { wanted: 1, .. })),
        Err(true)
    );
    for bad in ["000 10 0 start\nnonsense\n", "000 10 0 start\n\n", "000 10 0 read\n", "000 10 0 exit 69 70\n"] {
        assert!(matches!(Ending::of(bad), Err(Broken::Unreadable { .. })), "{bad:?}");
    }
}

/// A reading spells and reads back like every other crumb, and the harness's
/// own filter takes it out of the bring-up's order — which is what lets a
/// judge hold a trail with the PHY's readings in it to the same table.
#[test]
fn a_reading_is_a_crumb_the_bring_ups_order_leaves_out() {
    let step = Step::Saw { reg: regs::MDIC, value: 0x043f6020 };
    let line = crumbs::Line { seq: 0, at: 900, synced: 880, step };
    assert_eq!(line.to_string(), "000 900 880 saw MDIC 0x043f6020");
    let file = format!("{line}\n");
    assert_eq!(
        crumbs::lines(&file).map(|l| l.expect("the line it wrote").step).collect::<Vec<_>>(),
        [step]
    );
    assert_eq!(step.named().to_string(), "saw MDIC");
    assert!(crumbs::is_the_phys(&step.named().to_string()));
    assert!(crumbs::is_the_phys(
        &Step::Saw { reg: regs::EXTCNF_CTRL, value: 0 }.named().to_string()
    ));
    // A reading is the PHY's whatever register it names: only the PHY's own
    // steps leave one.
    assert!(crumbs::is_the_phys(&Step::Saw { reg: regs::CTRL, value: 0 }.named().to_string()));
    // An *access* to `CTRL` is the one the bring-up and the power step share.
    assert!(!crumbs::is_the_phys(&Step::Read { reg: regs::CTRL }.named().to_string()));
    assert!(crumbs::is_shared_with_the_phys(&Step::Read { reg: regs::CTRL }.named().to_string()));
    assert!(!crumbs::is_shared_with_the_phys(
        &Step::Read { reg: regs::CTRL_EXT }.named().to_string()
    ));
    assert!(!crumbs::is_shared_with_the_phys(&Step::Read { reg: regs::MDIC }.named().to_string()));
}

/// §8.2.1's `PHYPDN` and §8.2.2's `PHYPDEN` are the two bits the PCH's own
/// datasheet gives software over the PHY's power, and a bring-up that leaves
/// either standing is a bring-up whose MDI cycles never run: I219 §2.2's
/// Table 2-1 puts the interconnect in Electrical Idle while the PHY is down,
/// and §1.2 carries every MDIO access over that interconnect.
///
/// **The model's default is a part that comes up with both set**, so this is
/// also the negative control the rest of the PHY tests rest on: take the power
/// step out of the bring-up and every one of them stops at
/// [`PhyRefusal::MdiUnready`].
#[test]
fn the_power_step_is_what_lets_an_mdi_cycle_run_at_all() {
    let nic = Nic::i219(120);
    assert_ne!(nic.peek(regs::CTRL) & ctrl::PHY_POWER_DOWN, 0, "the part came up with it clear");
    assert_ne!(
        nic.peek(regs::CTRL_EXT) & regs::ctrl_ext::PHY_POWER_DOWN_ENABLE,
        0,
        "the part came up with it clear"
    );

    let driver = open(&nic);
    let phy = driver.brought_up().phy.expect("the PHY the power step reached");
    assert_eq!(phy.id >> 16, toyos_phy::IDENTIFIER_HIGH_INTEL as u32);
    assert_eq!(
        nic.peek(regs::CTRL) & ctrl::PHY_POWER_DOWN,
        0,
        "{}",
        nic.because("§8.2.1's PHYPDN is still standing")
    );
    assert_eq!(
        nic.peek(regs::CTRL_EXT) & regs::ctrl_ext::PHY_POWER_DOWN_ENABLE,
        0,
        "{}",
        nic.because("§8.2.2's PHYPDEN is still standing")
    );
    // Nothing was written into the two registers this driver only reads.
    assert_eq!(nic.peek(regs::PHY_CTRL), power::PHY_CTRL_RESET);
    assert!(!nic.written().contains(&regs::PHY_CTRL));
    assert!(!nic.written().contains(&regs::FWSM));
}

/// A part that does not take the write is refused rather than driven on the
/// assumption that it did — the same rule `IVAR` is read back under.
#[test]
fn a_mac_that_keeps_its_phy_down_is_refused_by_name() {
    let nic = Nic::i219(121);
    nic.refuses_writes_to(regs::CTRL);
    let (bar, clock, grant, line) = nic.parts();
    let driver = I219::open(nic.part(), bar, clock, grant, line).expect("the function opens");
    let why = driver.brought_up().phy.expect_err("a PHY held down is not brought up");
    assert!(
        matches!(why, PhyRefusal::PhyPoweredDown { ctrl, .. } if ctrl & ctrl::PHY_POWER_DOWN != 0),
        "{}",
        nic.because(&format!("it refused with {why:?}"))
    );
    assert_eq!(phy::Outcome::of(Err(why), Link::Down), phy::Outcome::PhyPoweredDown);
    assert!(why.to_string().contains("PHYPDN"));
}

/// §8.2.3: "The ME/Host should not issue new MDIC transactions while this bit
/// is set to 1. This bit is auto cleared by hardware after the transition has
/// occurred." So it is waited out, and a part that never finishes the
/// transition is refused with no command written into the register at all.
#[test]
fn an_interconnect_in_transition_is_waited_out_and_never_written_over() {
    let settled = Nic::i219(122);
    let driver = open(&settled);
    assert!(driver.brought_up().phy.is_ok(), "a transition that finishes is only a wait");

    let nic = Nic::i219(123);
    nic.interconnect_never_leaves_its_transition();
    let (bar, clock, grant, line) = nic.parts();
    let driver = I219::open(nic.part(), bar, clock, grant, line).expect("the function opens");
    let why = driver.brought_up().phy.expect_err("no transaction may be issued");
    assert!(
        matches!(why, PhyRefusal::MdiWaiting { .. }),
        "{}",
        nic.because(&format!("it refused with {why:?}"))
    );
    assert!(!nic.written().contains(&regs::MDIC), "{}", nic.because("a command was written"));
    assert_eq!(phy::Outcome::of(Err(why), Link::Down), phy::Outcome::MdiWaiting);
    // The interface is not kept: §4.5.2's release runs on this path too.
    assert_eq!(nic.peek(regs::EXTCNF_CTRL) & extcnf::MDIO_SW_OWNERSHIP, 0);
}

/// Each of §8.2's four readings decides exactly one thing, and the correction
/// is the two bits the document makes writable and nothing else.
#[test]
fn what_one_power_reading_decides() {
    let clear = power::Reading {
        ctrl: 0x0010_0008,
        ctrl_ext: 0,
        phy_ctrl: power::PHY_CTRL_RESET,
        fwsm: regs::fwsm::FIRMWARE_VALID,
        mdic: regs::mdic::READY,
    };
    assert_eq!(clear.unrouted(), None);
    assert!(!clear.phy_power_down());
    assert!(!clear.low_power_entry_enabled());
    assert!(clear.firmware_ready());
    assert!(!clear.interconnect_in_transition());
    assert!(clear.correction().nothing());
    assert_eq!(clear.correction().to_string(), "nothing was written");

    let down = power::Reading { ctrl: clear.ctrl | ctrl::PHY_POWER_DOWN, ..clear };
    assert!(down.phy_power_down());
    // The rest of the word is carried, and only bit 24 goes.
    assert_eq!(down.correction().ctrl, Some(clear.ctrl));
    assert_eq!(down.correction().ctrl_ext, None);
    assert!(down.to_string().contains("held in §8.2.1's power down"));

    let enabled = power::Reading {
        ctrl_ext: 0x0000_0040 | regs::ctrl_ext::PHY_POWER_DOWN_ENABLE,
        ..clear
    };
    assert!(enabled.low_power_entry_enabled());
    assert_eq!(enabled.correction().ctrl, None);
    assert_eq!(enabled.correction().ctrl_ext, Some(0x0000_0040));

    let mute = power::Reading { fwsm: 0, ..clear };
    assert!(!mute.firmware_ready());
    // The firmware's own report decides nothing: §8.2.9 attaches nothing to it.
    assert!(mute.correction().nothing());
    assert!(mute.to_string().contains("reports itself not ready"));

    let moving = power::Reading { mdic: regs::mdic::WAIT, ..clear };
    assert!(moving.interconnect_in_transition());
    assert!(moving.correction().nothing());

    // A window that stopped decoding is a reading no correction comes out of.
    for gone in [
        power::Reading { ctrl: u32::MAX, ..clear },
        power::Reading { ctrl_ext: u32::MAX, ..clear },
        power::Reading { phy_ctrl: u32::MAX, ..clear },
        power::Reading { fwsm: u32::MAX, ..clear },
        power::Reading { mdic: u32::MAX, ..clear },
    ] {
        assert_eq!(gone.unrouted(), Some(u32::MAX));
    }

    let settled = power::Settled { before: down, wrote: down.correction(), after: clear };
    assert!(settled.settled());
    assert!(settled.to_string().contains("CTRL was written"));
    assert!(!power::Settled { after: down, ..settled }.settled());
    assert!(
        !power::Settled { after: enabled, ..settled }.settled(),
        "a low-power entry left enabled is not settled either"
    );
}

// --- the wake and the full reset ---

use crate::stub::Lcd;
use crate::wake::{Action, Answer, Moment};

fn woke_of(driver: &Driver, nic: &Nic) -> wake::Woke {
    match driver.brought_up().woke {
        Some(Ok(woke)) => woke,
        other => panic!("{}", nic.because(&format!("the wake did not run: {other:?}"))),
    }
}

fn moments(woke: &wake::Woke) -> Vec<(Moment, bool)> {
    woke.asked().map(|(moment, answer)| (moment, answer.answered())).collect()
}

/// A PHY the agent before this driver left out of `MDIC`'s reach — powered
/// down, and coming back on SMBus once power-cycled — is climbed to one rung
/// at a time, each rung asked, and taken back to PCIe before the reset; and the
/// reset that follows takes the PHY with the MAC and leaves it answering.
#[test]
fn a_phy_left_out_of_reach_is_climbed_to_before_the_reset() {
    let nic = Nic::i219(101);
    nic.set_link(true);
    let driver = open(&nic);
    let woke = woke_of(&driver, &nic);
    assert_eq!(
        moments(&woke),
        [
            (Moment::AsFound, false),
            (Moment::PowerCycled, false),
            (Moment::SmbusForced, true),
            (Moment::BackOnPcie, true),
        ],
        "{}",
        nic.because(&format!("the ladder was not climbed in the host driver's order: {woke}"))
    );
    assert!(matches!(woke.asked().next(), Some((_, Answer::Silent { .. }))));
    assert_eq!(nic.power_cycles(), 1);
    assert_eq!(nic.phy_resets(), 1, "{}", nic.because("the reset did not take the PHY"));
    let reset = driver.brought_up().reset.expect("the I219's reset is the full one");
    assert!(reset.phy_reset && reset.flag.is_ok() && reset.init_done_after_nanos.is_some());
    assert_eq!(nic.lcd(), Lcd::InStep);
    assert!(driver.brought_up().phy.is_ok(), "{}", nic.because("the bring-up missed the PHY"));

    // Where the power cycle brings it back on PCIe, the ladder ends there.
    let nic = Nic::with(
        102,
        Part::I219,
        Permits { phy_comes_back_on_smbus: false, ..Permits::default() },
    );
    let driver = open(&nic);
    assert_eq!(
        moments(&woke_of(&driver, &nic)),
        [(Moment::AsFound, false), (Moment::PowerCycled, true)]
    );
    assert!(driver.brought_up().phy.is_ok());
}

/// A PHY that answers as found is asked once and never power-cycled.
#[test]
fn a_phy_in_reach_is_asked_once_and_left_powered() {
    let nic = Nic::with(
        103,
        Part::I219,
        Permits { phy_starts_out_of_reach: false, ..Permits::default() },
    );
    let driver = open(&nic);
    assert_eq!(moments(&woke_of(&driver, &nic)), [(Moment::AsFound, true)]);
    assert_eq!(nic.power_cycles(), 0);
    assert_eq!(nic.phy_resets(), 1);
    assert!(driver.brought_up().phy.is_ok());
}

/// Where `FWSM` says the firmware blocks a PHY reset, neither `PHY_RST` nor a
/// `LANPHYPC` cycle goes out — the host driver reports both "blocked by ME" on
/// that bit — and a MAC reset alone then leaves a PHY that answered before it
/// out of reach, which the bring-up refuses by name.
#[test]
fn a_phy_reset_the_firmware_blocks_is_never_issued() {
    let nic = Nic::with(
        104,
        Part::I219,
        Permits {
            phy_starts_out_of_reach: false,
            firmware_allows_a_phy_reset: false,
            ..Permits::default()
        },
    );
    let driver = open(&nic);
    assert_eq!(nic.phy_resets(), 0);
    assert_eq!(nic.power_cycles(), 0);
    assert!(!driver.brought_up().reset.expect("the I219's reset").phy_reset);
    assert!(matches!(driver.brought_up().phy, Err(PhyRefusal::MdiUnready { .. })));

    // And out of reach as well: the rungs that need a PHY reset are skipped.
    let nic = Nic::with(
        105,
        Part::I219,
        Permits { firmware_allows_a_phy_reset: false, ..Permits::default() },
    );
    let driver = open(&nic);
    assert_eq!(nic.power_cycles(), 0);
    assert_eq!(
        moments(&woke_of(&driver, &nic)),
        [(Moment::AsFound, false), (Moment::SmbusForced, false), (Moment::SmbusReleased, false)]
    );
}

/// A PHY no rung reaches is refused after the reset by the wall it is, with
/// every rung's ask beside it — and the MAC is not left on SMBus.
#[test]
fn a_phy_no_rung_reaches_is_refused_after_the_reset() {
    let nic = Nic::i219(106);
    nic.mdi_never_ready();
    let driver = open(&nic);
    let woke = woke_of(&driver, &nic);
    assert_eq!(woke.asked().count(), 1 + wake::LADDER.len());
    assert!(woke.answered().is_none());
    assert!(woke.asked().all(|(_, answer)| matches!(answer, Answer::Silent { .. })));
    assert_eq!(nic.peek(regs::CTRL_EXT) & regs::ctrl_ext::FORCE_SMBUS, 0);
    assert!(matches!(driver.brought_up().phy, Err(PhyRefusal::MdiUnready { .. })));
}

fn no_arbitration_nearer_than_the_pace(nic: &Nic) -> bool {
    nic.arbitration_at().windows(2).all(|pair| pair[1] - pair[0] >= toyos_phy::ARBITRATION_PACE_NANOS)
}

/// Every ask is witnessed: its own crumb ahead of it and `MDIC`'s word behind
/// every transaction it made, so a trail says in the part's words how each
/// ended.
#[test]
fn every_ask_leaves_the_parts_own_answer_on_the_trail() {
    let nic = Nic::i219(109);
    let (bar, clock, grant, line) = nic.parts();
    let seen: Seens = Rc::default();
    let trail = Runs::over(Noted(Rc::clone(&seen)));
    I219::open_trailing(Part::I219, Crumbed::over(bar, &trail), clock, grant, line, &trail)
        .unwrap_or_else(|why| panic!("{}", nic.because(&format!("open refused: {why}"))));
    let crumbs = crumbs_of(&seen.borrow());
    let asks: Vec<usize> = crumbs
        .iter()
        .enumerate()
        .filter(|(_, step)| matches!(step, Step::Ask { .. }))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(asks.len(), 4, "{}", nic.because("the ladder asked a different number of times"));
    for at in asks {
        let behind = crumbs[at + 1..]
            .iter()
            .take_while(|step| !matches!(step, Step::Ask { .. } | Step::Opened))
            .find(|step| matches!(step, Step::Saw { reg: regs::MDIC, .. }));
        assert!(behind.is_some(), "{}", nic.because(&format!("{} left no answer", crumbs[at])));
    }
    assert!(no_arbitration_nearer_than_the_pace(&nic));
}

#[test]
fn an_ask_spells_and_reads_back_at_every_moment() {
    for moment in Moment::ALL {
        let step = Step::Ask { moment };
        let line = crumbs::Line { seq: 0, at: 5, synced: 3, step };
        let file = format!("{line}\n");
        assert_eq!(
            crumbs::lines(&file).map(|l| l.expect("the line it wrote").step).collect::<Vec<_>>(),
            [step]
        );
        assert!(crumbs::is_the_phys(&step.named().to_string()));
    }
    assert!(crumbs::is_the_phys(&Step::Write { reg: regs::FEXTNVM3, value: 0 }.named().to_string()));
    assert!(crumbs::is_shared_with_the_phys("read STATUS"));
    assert!(!crumbs::is_shared_with_the_phys("write STATUS"));
}

/// What the wake and the full reset write, as bit arithmetic on what was read.
#[test]
fn what_the_wake_and_the_full_reset_write() {
    use regs::{ctrl, ctrl_ext, fextnvm3, fwsm};
    let held = 0x0018_0244 | ctrl::LANPHYPC_VALUE;
    let low = wake::lanphypc_low(held);
    assert_eq!(low & (ctrl::LANPHYPC_OVERRIDE | ctrl::LANPHYPC_VALUE), ctrl::LANPHYPC_OVERRIDE);
    assert_eq!(low & !(ctrl::LANPHYPC_OVERRIDE | ctrl::LANPHYPC_VALUE), 0x0018_0244);
    assert_eq!(wake::lanphypc_released(low), 0x0018_0244, "only the override goes");

    let counter = wake::phy_cfg_counter(0xFFFF_FFFF);
    assert_eq!(counter & fextnvm3::PHY_CFG_COUNTER_MASK, fextnvm3::PHY_CFG_COUNTER_50MS);
    assert_eq!(counter | fextnvm3::PHY_CFG_COUNTER_MASK, 0xFFFF_FFFF, "every other bit carried");

    // The T14's own words: CTRL_EXT 0x815a1027 and FWSM 0x60000040.
    let ext = 0x815a_1027;
    assert_eq!(wake::smbus_forced(ext), ext | ctrl_ext::FORCE_SMBUS);
    assert_eq!(wake::smbus_released(wake::smbus_forced(ext)), ext);
    assert!(wake::power_cycle_done(ext), "the T14 read the cycle-done bit set");
    assert!(wake::phy_reset_allowed(0x6000_0040));
    assert_eq!(wake::reset_word(0x0018_0244, 0x6000_0040), 0x0018_0244 | ctrl::RST | ctrl::PHY_RST);
    assert_eq!(wake::reset_word(0x0018_0244, fwsm::FIRMWARE_VALID), 0x0018_0244 | ctrl::RST);

    assert!(wake::rung_allowed(Action::PowerCycle, 0x6000_0040));
    assert!(!wake::rung_allowed(Action::PowerCycle, 0));
    assert!(wake::rung_allowed(Action::ForceSmbus, 0));
    assert!(wake::rung_allowed(Action::ReleaseSmbus, 0));
    assert_eq!(wake::phy_smbus_released(0x0013), 0x0012);
}

// --- the PCH's own bits, host wake-up, the counts, and the lease probe ---

/// The PCH's MAC is given every bit its own host driver writes before the
/// rings, and the 82574 — whose datasheet this driver is otherwise written
/// from — is given none of them.
#[test]
fn the_pchs_mac_gets_its_host_drivers_bits_and_the_82574_none() {
    use regs::{ctrl_ext, fflt_dbg, gcr, pbeccsts, rfctl, tarc, txdctl};
    let nic = Nic::i219(130);
    let _driver = open(&nic);
    let has = |reg, bits: u32| nic.peek(reg) & bits == bits;
    assert!(has(
        regs::CTRL_EXT,
        ctrl_ext::REQUIRED_22 | ctrl_ext::DRIVER_LOADED | ctrl_ext::RELAXED_ORDERING_DISABLE
    ));
    assert_eq!(nic.peek(regs::TXDCTL), txdctl::PCH, "{}", nic.because("the first queue"));
    assert_eq!(nic.peek(regs::TXDCTL1), txdctl::PCH, "{}", nic.because("the second queue"));
    assert!(has(regs::TARC0, tarc::TARC0_REQUIRED));
    // `TCTL` as this driver writes it has Multiple Request Support clear, so
    // `TARC1`'s bit 28 is set.
    assert_eq!(nic.peek(regs::TCTL) & tctl::MULR, 0);
    assert!(has(regs::TARC1, tarc::TARC1_REQUIRED | tarc::TARC1_SINGLE_REQUEST));
    assert!(has(regs::RFCTL, rfctl::NFS_FILTERS_OFF));
    assert!(has(regs::PBECCSTS, pbeccsts::ECC_ENABLE));
    assert!(has(regs::CTRL, ctrl::MEHE));
    assert!(has(regs::FFLT_DBG, fflt_dbg::DONT_GATE_WAKE_DMA_CLOCK));
    assert_eq!(nic.peek(regs::GCR) & gcr::NO_SNOOP, 0);
    assert_eq!(nic.peek(regs::WUC), 0);
    for reg in [regs::WUC, regs::GCR] {
        assert!(nic.written().contains(&reg), "{}", nic.because(&format!("{reg:#x} unwritten")));
    }

    let nic = Nic::new(131);
    let _driver = open(&nic);
    assert_eq!(nic.peek(regs::TXDCTL), txdctl::SUGGESTED);
    for reg in [
        regs::TXDCTL1,
        regs::TARC0,
        regs::TARC1,
        regs::RFCTL,
        regs::PBECCSTS,
        regs::WUC,
        regs::GCR,
        regs::FFLT_DBG,
    ] {
        assert!(
            !nic.written().contains(&reg),
            "{}",
            nic.because(&format!("the 82574 had the PCH's {reg:#x} written"))
        );
    }
    assert_eq!(nic.peek(regs::CTRL) & ctrl::MEHE, 0);
}

/// `TARC1`'s bit 28 follows `TCTL`'s Multiple Request Support and nothing
/// else in the register moves.
#[test]
fn tarc1_follows_multiple_request_support() {
    use regs::tarc;
    let set = crate::pch::tarc1(0, 0);
    assert_eq!(set, tarc::TARC1_REQUIRED | tarc::TARC1_SINGLE_REQUEST);
    let cleared = crate::pch::tarc1(tarc::TARC1_SINGLE_REQUEST | 0x3, tctl::MULR);
    assert_eq!(cleared, tarc::TARC1_REQUIRED | 0x3);
}

/// I219 §7.4: a PHY an earlier operating system armed for host wake-up keeps
/// `Host_WU_Active` through every reset but a power cycle (§9.5.3.2), and the
/// host clears it behind the LCD reset. The bring-up says what it found.
#[test]
fn host_wake_up_left_armed_is_cleared_behind_the_reset() {
    use toyos_phy::port_general;
    // In reach as found, so nothing power-cycles it and the bit survives to
    // the bring-up.
    let nic = Nic::with(
        132,
        Part::I219,
        Permits { phy_starts_out_of_reach: false, ..Permits::default() },
    );
    let driver = open(&nic);
    let phy = driver.brought_up().phy.expect("the PHY in reach");
    assert_ne!(phy.port_general & port_general::HOST_WAKE_UP_ACTIVE, 0, "found armed");
    assert_eq!(
        nic.phy_port_general() & port_general::HOST_WAKE_UP_ACTIVE,
        0,
        "{}",
        nic.because("host wake-up was left armed")
    );
    assert_eq!(
        nic.phy_port_general() | port_general::HOST_WAKE_UP_ACTIVE,
        phy.port_general,
        "{}",
        nic.because("another field of the register moved")
    );
    assert!(phy.to_string().contains("was cleared"), "{phy}");

    // A power cycle is a power-on reset, and then there is nothing to clear.
    let nic = Nic::i219(133);
    let driver = open(&nic);
    assert_eq!(nic.power_cycles(), 1);
    let phy = driver.brought_up().phy.expect("the PHY the ladder reached");
    assert_eq!(phy.port_general & port_general::HOST_WAKE_UP_ACTIVE, 0);
    assert!(!phy.to_string().contains("was cleared"), "{phy}");
}

/// What the driver counts and what the MAC's own statistics count agree on a
/// part that moves every frame it is given, and the statistics — cleared by
/// every read — add up across calls.
#[test]
fn the_driver_and_the_macs_statistics_count_the_same_frames() {
    let nic = Nic::new(134);
    let mut driver = open(&nic);
    nic.set_link(true);
    for tag in 1..=3u8 {
        let payload = frame(tag, 100);
        let slot = driver.tx_reserve(payload.len()).expect("a free transmit descriptor");
        nic.put_bytes(slot.at, &payload);
        driver.tx_commit(slot);
    }
    nic.run();
    assert_eq!(nic.sent().len(), 3);
    nic.deliver(&frame(9, 64));
    nic.deliver(&frame(10, 64));
    one_pass(&mut driver);
    nic.run();
    let got = drain(&nic, &mut driver);
    assert_eq!(got.len(), 2);
    driver.reclaim();
    let counters = driver.counters();
    assert_eq!((counters.sent, counters.received), (3, 2), "{}", nic.because("the driver's count"));
    assert_eq!(counters.anomalies(), Counters::default(), "a working part is no anomaly");
    let wire = driver.wire();
    assert_eq!(
        (wire.sent, wire.received, wire.seen, wire.missed, wire.crc_errors),
        (3, 2, 2, 0, 0),
        "{}",
        nic.because("the MAC's count")
    );
    // Read to clear: a second reading adds nothing that did not happen.
    assert_eq!(driver.wire(), wire);
    nic.deliver(&frame(11, 64));
    nic.run();
    assert_eq!(driver.wire().seen, 3);
}

/// Every verdict the lease probe can exit with reads back to itself, and none
/// of them is a code a panicking netd or an ordinary exit ends with.
#[test]
fn every_lease_verdict_has_one_exit_code_that_reads_back() {
    use crate::lease::{Verdict, LEASED};
    use toyos_phy::Outcome;
    assert_eq!(Verdict::Leased.exit_code(), LEASED);
    let mut codes = vec![LEASED];
    for outcome in Outcome::ALL {
        let verdict = Verdict::NotLeased(outcome);
        assert_eq!(Verdict::from_exit_code(verdict.exit_code()), Some(verdict));
        codes.push(verdict.exit_code());
    }
    assert_eq!(Verdict::from_exit_code(LEASED), Some(Verdict::Leased));
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), Outcome::ALL.len() + 1, "two verdicts share a code");
    for foreign in [0, 1, 101, 139] {
        assert_eq!(Verdict::from_exit_code(foreign), None, "{foreign}");
    }
}

/// The report's lines spell and read back, and a file of them sums up to the
/// lease, the last counts and the exit — with a torn last line left out.
#[test]
fn a_lease_report_reads_back_as_it_was_written() {
    use crate::lease::{self, Counts, Event, Line};
    use core::net::Ipv4Addr;
    let lease = Event::Leased {
        address: Ipv4Addr::new(192, 168, 1, 46),
        prefix: 24,
        server: Ipv4Addr::new(192, 168, 1, 1),
        router: Some(Ipv4Addr::new(192, 168, 1, 1)),
    };
    let counts = Counts {
        sent: 4,
        received: 9,
        wire: Wire { sent: 4, received: 9, seen: 12, missed: 1, crc_errors: 0 },
    };
    let events = [
        Event::BroughtUp("the MAC and the PHY were reset together"),
        Event::Link(Link::Up { speed: Speed::Mbps10, full_duplex: true }),
        Event::Link(Link::Down),
        lease,
        Event::Leased {
            address: Ipv4Addr::new(10, 0, 2, 15),
            prefix: 24,
            server: Ipv4Addr::new(10, 0, 2, 2),
            router: None,
        },
        Event::Counts(counts),
        Event::Exit { code: lease::LEASED },
    ];
    let mut file = std::string::String::new();
    for (ms, event) in events.iter().enumerate() {
        let line = Line { ms: ms as u64 * 100, event: *event };
        let text = line.to_string();
        assert_eq!(Line::parse(&text), Some(line), "{text}");
        file.push_str(&text);
        file.push('\n');
    }
    assert_eq!(
        Line { ms: 300, event: lease }.to_string(),
        "300 leased 192.168.1.46/24 from 192.168.1.1 router 192.168.1.1"
    );
    let summary = lease::summary(&format!("{file}700 exit 6")).expect("a whole report");
    assert_eq!(summary.lease, Some((300, lease)), "the first lease is the one recorded");
    assert_eq!(summary.counts, Some(counts));
    assert_eq!(summary.exit, Some(lease::LEASED), "the torn line is not the exit");
    for bad in ["x link up\n", "5 link up 10\n", "5 leased 1.2.3.4 from 1.2.3.5 router none\n", "5 exit\n"] {
        assert!(lease::summary(bad).is_err(), "{bad:?}");
    }
}
