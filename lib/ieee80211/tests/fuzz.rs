//! **Structured fuzzing of the boundary**, a fixed count of seeded inputs, so a red names its
//! iteration.
//!
//! Frames are drawn from the grammar a radio hands over — beacons and the other management bodies
//! with lists of typed and unknown elements, BlockAckReq and BlockAck, data frames carrying LLC/SNAP
//! MSDUs or A-MSDUs, protected or not — and then sometimes truncated, bent, lengthened or given a
//! huge length. Every one is read through every layer and ends in a value or a named refusal. One
//! left unbent reads back as what was drawn. The reorder window is driven with sequence numbers
//! around a moving base and passes every accepted frame up once, in order.

mod common;

use std::collections::BTreeMap;

use common::{element, Rng, AP, STA};
use toyos_ieee80211::data::{llc_snap, msdus, CcmpHeader, Ethernet};
use toyos_ieee80211::element::Elements;
use toyos_ieee80211::frame::Frame;
use toyos_ieee80211::mgmt::Body;
use toyos_ieee80211::reorder::{Received, Window};
use toyos_ieee80211::{Address, Refused, Seq};

const FUZZ_ITERATIONS: u64 = 20_000;

/// The Ethernet frames a data frame carries: destination, source, EtherType and payload.
type Sent = Vec<(Address, Address, u16, Vec<u8>)>;

/// One element: often typed and well formed, sometimes bent, sometimes unknown.
fn draw_element(rng: &mut Rng) -> Vec<u8> {
    let suite = |rng: &mut Rng| [0x00, 0x0F, 0xAC, rng.below(14) as u8];
    match rng.below(18) {
        0 => element(0, &rng.bytes(34)),
        1 => element(1, &rng.bytes(9)),
        2 => element(50, &rng.bytes(12)),
        3 => element(3, &rng.bytes(2)),
        4 => {
            let mut body = vec![rng.below(4) as u8, rng.below(4) as u8, rng.next() as u8];
            body.extend(rng.bytes(8));
            element(5, &body)
        }
        5 => {
            let mut body = [b"US ", b"DE\x04", b"jp ", b"GB "][rng.below(4)].to_vec();
            for _ in 0..rng.below(5) {
                body.extend([[1, 13, 20], [36, 4, 23], [190, 12, 30], [201, 81, 0], [0, 1, 1]][rng.below(5)]);
            }
            // The pad an odd length needs, sometimes left out or added where none is needed.
            if (body.len() % 2 == 1) != rng.chance(10) {
                body.push(0);
            }
            element(7, &body)
        }
        6 => {
            let mut body = vec![1, 0];
            body.extend(suite(rng));
            for _ in 0..2 {
                let n = rng.below(3);
                body.extend((n as u16).to_le_bytes());
                (0..n).for_each(|_| body.extend(suite(rng)));
            }
            body.extend([[0x00, 0x00], [0xC0, 0x00], [0x80, 0x00], [0x40, 0x00]][rng.below(4)]);
            body.truncate(rng.below(body.len() + 1).max(2));
            element(48, &body)
        }
        7 => element(244, &[rng.below(4) as u8 | 0x20, rng.next() as u8][..rng.below(2) + 1]),
        8 => element(45, &rng.bytes(27)),
        9 => {
            let mut body = rng.bytes(22);
            body.resize(22, 0);
            element(61, &body)
        }
        10 => element(191, &rng.bytes(13)),
        11 => element(192, &[rng.below(5) as u8, 42, 0, 0xFC, 0xFF]),
        12 => element(127, &rng.bytes(10)),
        13 => {
            let phy0 = [0, 0x08, 0x18, 0x10][rng.below(4)];
            let ppe = rng.chance(40);
            let mut body = vec![35];
            body.extend((0..6).map(|_| rng.next() as u8));
            // PHY Capabilities: B3 and B4 in octet 0, PPE Thresholds Present (B55) in octet 6.
            body.extend([phy0, 0, rng.next() as u8, 0, 0, 0, if ppe { 0x80 } else { 0 }, 0, 0, 0, 0]);
            body.extend(vec![0xFA; 4 + 4 * (phy0.count_ones() as usize)]);
            if ppe {
                let first = rng.next() as u8 & 0x7F;
                let bits = 7 + 6 * ((first & 7) as usize + 1) * (first >> 3).count_ones() as usize;
                body.push(first);
                body.extend(vec![0; bits.div_ceil(8) - 1]);
            }
            element(255, &body)
        }
        14 => {
            let present = [0u32, 1 << 14, 1 << 15, 1 << 17, 1 << 14 | 1 << 15 | 1 << 17][rng.below(5)];
            let p = present.to_le_bytes();
            let mut body = vec![36, p[0], p[1], p[2], rng.next() as u8, 0xFC, 0xFF];
            let extra = 3 * usize::from(present & 1 << 14 != 0) + usize::from(present & 1 << 15 != 0) + 5 * usize::from(present & 1 << 17 != 0);
            body.extend((0..extra).map(|_| rng.next() as u8));
            element(255, &body)
        }
        15 | 16 => {
            let mut body = vec![0x00, 0x50, 0xF2, 0x02, rng.below(3) as u8, 1, rng.next() as u8];
            if body[4] == 1 {
                body.push(0);
                let mut acis = [0u8, 1, 2, 3];
                if rng.chance(10) {
                    acis[3] = acis[rng.below(3)];
                }
                for aci in acis {
                    body.extend([aci << 5 | (2 + rng.below(14)) as u8, rng.next() as u8, rng.next() as u8, 0]);
                }
            }
            element(221, &body)
        }
        _ => {
            let id = rng.next() as u8;
            if id == 255 { element(255, &rng.bytes(4)) } else { element(id, &rng.bytes(20)) }
        }
    }
}

/// A management frame: Frame Control's subtype, a body of its fixed fields, and maybe elements.
fn draw_management(rng: &mut Rng) -> Vec<u8> {
    let subtype = [8, 5, 4, 11, 12, 10, 0, 1, 2, 3, 13, 14][rng.below(12)];
    let mut f = common::management(subtype, &[]);
    f.extend((0..12).map(|_| rng.next() as u8));
    if subtype == 8 || subtype == 5 {
        f[32..34].copy_from_slice(&100u16.to_le_bytes());
    }
    for _ in 0..rng.below(8) {
        f.extend(draw_element(rng));
    }
    f
}

fn draw_block_ack(rng: &mut Rng) -> Vec<u8> {
    let bar = rng.chance(50);
    let mut f = vec![if bar { 0x84 } else { 0x94 }, 0, 0, 0];
    f.extend(STA.0);
    f.extend(AP.0);
    f.extend([if rng.chance(80) { 0x04 } else { rng.next() as u8 }, (rng.below(16) << 4) as u8]);
    let (fragment, octets) = if bar { (0, 0) } else { [(0, 8), (4, 32)][rng.below(2)] };
    f.extend(((rng.below(4096) << 4) as u16 | fragment).to_le_bytes());
    f.extend((0..octets).map(|_| rng.next() as u8));
    f
}

/// A data frame from the access point and the Ethernet frames it was built from.
fn draw_data(rng: &mut Rng) -> (Vec<u8>, Sent) {
    let ethertypes = [0x0800, 0x0806, 0x86DD, 0x888E, 0x8137, 0x80F3];
    let mut sent = Vec::new();
    let amsdu = rng.chance(40);
    let protected = rng.chance(30);
    let mut body = Vec::new();
    if protected {
        let pn = rng.next() & 0xFFFF_FFFF_FFFF;
        let p = pn.to_le_bytes();
        body.extend([p[0], p[1], 0, 0x20 | (rng.below(4) as u8) << 6, p[2], p[3], p[4], p[5]]);
    }
    let count = if amsdu { 1 + rng.below(4) } else { 1 };
    for i in 0..count {
        let (da, sa) = if amsdu { (rng.address(), rng.address()) } else { (STA, common::PEER) };
        let ethertype = ethertypes[rng.below(ethertypes.len())];
        let payload = rng.bytes(60);
        let msdu = [&llc_snap(ethertype).unwrap()[..], &payload].concat();
        if amsdu {
            body.extend(common::subframe(da, sa, &msdu, i + 1 == count));
        } else {
            body.extend(&msdu);
        }
        sent.push((da, sa, ethertype, payload));
    }
    let mut f = common::qos_from_ds(amsdu, &body);
    if protected {
        f[1] |= 0x40;
    }
    (f, sent)
}

/// Bend `frame` one way, or not at all; whether it changed.
fn mutate(rng: &mut Rng, frame: &mut Vec<u8>) -> bool {
    if !rng.chance(40) || frame.is_empty() {
        return false;
    }
    match rng.below(4) {
        0 => frame.truncate(rng.below(frame.len())),
        1 => {
            let at = rng.below(frame.len());
            frame[at] ^= 1 + rng.below(255) as u8;
        }
        2 => frame.extend(rng.bytes(8).into_iter().chain([rng.next() as u8])),
        _ => {
            let at = rng.below(frame.len());
            frame[at] = 0xFF;
        }
    }
    true
}

/// Read `frame` through every layer; the first refusal, or the Ethernet frames it carries.
fn read(frame: &[u8]) -> Result<Sent, Refused> {
    match Frame::parse(frame)? {
        Frame::Management(m) => {
            let elements = match Body::parse(m.subtype, m.body)? {
                Body::Beacon(b) | Body::ProbeResponse(b) => b.elements,
                Body::ProbeRequest { elements } => elements,
                Body::AssociationRequest(a) | Body::ReassociationRequest(a) => a.elements,
                Body::AssociationResponse(a) | Body::ReassociationResponse(a) => a.elements,
                _ => return Ok(Vec::new()),
            };
            let e = Elements::parse(elements)?;
            if let Some(tim) = e.tim {
                (0..=2008).for_each(|aid| _ = tim.buffered(aid));
            }
            if let Some(country) = e.country {
                country.triplets().for_each(drop);
            }
            Ok(Vec::new())
        }
        Frame::BlockAck(ba) => {
            (0..4096).for_each(|s| _ = ba.acked(Seq::new(s)));
            Ok(Vec::new())
        }
        Frame::BlockAckReq(_) => Ok(Vec::new()),
        Frame::Data(d) => {
            let plaintext = if d.flags.protected { CcmpHeader::split(d.body)?.1 } else { d.body };
            let mut out = Vec::new();
            for msdu in msdus(&d, plaintext)? {
                let eth = Ethernet::from_msdu(msdu?)?;
                out.push((eth.dst, eth.src, eth.ethertype, eth.payload.to_vec()));
            }
            Ok(out)
        }
    }
}

#[test]
fn every_drawn_frame_is_read_or_refused_by_name_and_an_unbent_one_reads_back() {
    let mut outcomes: BTreeMap<String, usize> = BTreeMap::new();
    let mut read_back = 0;
    for iteration in 0..FUZZ_ITERATIONS {
        let mut rng = Rng(iteration);
        let (mut frame, sent) = match rng.below(3) {
            0 => (draw_management(&mut rng), None),
            1 => (draw_block_ack(&mut rng), None),
            _ => {
                let (f, sent) = draw_data(&mut rng);
                (f, Some(sent))
            }
        };
        let bent = mutate(&mut rng, &mut frame);
        let result = read(&frame);
        let key = match &result {
            Ok(_) => "read".to_string(),
            Err(e) => format!("{e:?}"),
        };
        *outcomes.entry(key).or_default() += 1;
        if let (Some(sent), false) = (sent, bent) {
            assert_eq!(result.as_ref().ok(), Some(&sent), "iteration {iteration}: an unbent data frame");
            read_back += 1;
        }
    }
    println!("{FUZZ_ITERATIONS} frames, {read_back} read back: {outcomes:#?}");
    assert!(read_back > FUZZ_ITERATIONS as usize / 5, "the fuzz reads data frames back");
    assert!(outcomes.len() > 32, "the fuzz reaches most refusals: {}", outcomes.len());
}

#[test]
fn the_reorder_window_passes_every_accepted_frame_up_once_and_in_order() {
    for iteration in 0..FUZZ_ITERATIONS / 10 {
        let mut rng = Rng(1 << 40 | iteration);
        let size = 1 + rng.below(256) as u16;
        let first = rng.below(4096) as u32;
        let mut w = Window::new(Seq::new(first as u16), size).unwrap();
        let mut base = first;
        let mut accepted = 0;
        let mut out: Vec<u32> = Vec::new();
        for _ in 0..rng.below(300) {
            if rng.chance(5) {
                base += rng.below(size as usize * 2) as u32;
                w.release_to(Seq::new(base as u16), &mut out);
                continue;
            }
            let position = (base + rng.below(size as usize * 2) as u32).saturating_sub(rng.below(size as usize) as u32);
            if w.receive(Seq::new(position as u16), position, &mut out) == Received::Taken {
                accepted += 1;
            }
            base = base.max(position.saturating_sub(size as u32 - 1));
        }
        w.release_to(w.start().add(2047), &mut out);
        assert!(out.windows(2).all(|p| p[0] < p[1]), "iteration {iteration}: out of order: {out:?}");
        assert_eq!(out.len(), accepted, "iteration {iteration}: a frame lost or passed up twice");
    }
}
