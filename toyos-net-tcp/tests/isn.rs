//! Initial sequence numbers and timestamp offsets. The expected values are the
//! specification's, computed from the SipHash paper by the reader, not by this crate.

mod common;

use common::*;
use toyos_net_tcp::{isn, ts_offset, Instant, Tcp, Tuple};
use toyos_net_wire::siphash::siphash24;

fn tuple(local_port: u16, remote_port: u16) -> Tuple {
    Tuple { local: ep(A, local_port), remote: ep(B, remote_port) }
}

fn descending() -> [u8; 16] {
    core::array::from_fn(|i| 0xff - i as u8)
}

/// The SYN a fresh stack sends for `tuple` at `now`.
fn syn(now: Instant, tuple: Tuple) -> O {
    let mut tcp = Tcp::new(config(65_535)).unwrap();
    tcp.connect(now, tuple.local.addr, Some(tuple.local.port), tuple.remote).unwrap();
    let mut out = Vec::new();
    tcp.transmit(now, 1, |o| out.push(datagram(o)));
    parse_out(&out[0], 0)
}

#[test]
fn s_isn_001_siphash_reference_vectors() {
    assert_eq!(siphash24(&key(0), &[]), 0x726f_db47_dd0e_0e31);
    let message: Vec<u8> = (0..15).collect();
    assert_eq!(siphash24(&key(0), &message), 0xa129_ca61_49be_45e5);
}

#[test]
fn s_isn_002_isn_at_clock_zero() {
    let bytes = [0xc0, 0x00, 0x02, 0x01, 0xc0, 0x00, 0xc0, 0x00, 0x02, 0x02, 0x00, 0x50];
    assert_eq!(siphash24(&key(0), &bytes), 0xa7ea_5474_6827_6d3b);
    assert_eq!(isn(&key(0), &tuple(49152, 80), Instant::from_nanos(0)).get(), 0x6827_6d3b);
    assert_eq!(syn(Instant::from_nanos(0), tuple(49152, 80)).seq, 0x6827_6d3b, "the SYN carries it");
}

#[test]
fn s_isn_003_the_4us_clock() {
    assert_eq!(isn(&key(0), &tuple(49152, 80), Instant::from_millis(1_000)).get(), 0x682b_3dcb);
}

#[test]
fn s_isn_004_every_field_of_the_tuple_counts() {
    assert_eq!(isn(&key(0), &tuple(49152, 81), Instant::from_nanos(0)).get(), 0x86be_e707);
    assert_eq!(isn(&key(0), &tuple(49153, 80), Instant::from_nanos(0)).get(), 0xa9a8_6a46);
}

#[test]
fn s_isn_005_the_key_counts() {
    assert_eq!(isn(&descending(), &tuple(49152, 80), Instant::from_nanos(0)).get(), 0x6fdc_9f08);
}

#[test]
fn s_isn_006_clock_wrap() {
    let last = Instant::from_nanos(((1u64 << 32) - 1) * 4_000);
    assert_eq!(isn(&key(0), &tuple(49152, 80), last).get(), 0x6827_6d3a);
    let wrapped = Instant::from_nanos((1u64 << 32) * 4_000);
    assert_eq!(isn(&key(0), &tuple(49152, 80), wrapped).get(), 0x6827_6d3b);
}

/// The unpredictability test: the `mutate-isn-constant` control reds it.
#[test]
fn s_isn_007_prop_unpredictable() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let now = Instant::from_nanos(0);
    let mut across_keys: Vec<u32> = (0..64)
        .map(|_| {
            let mut k = [0u8; 16];
            k[..8].copy_from_slice(&next().to_le_bytes());
            k[8..].copy_from_slice(&next().to_le_bytes());
            isn(&k, &tuple(49152, 80), now).get()
        })
        .collect();
    across_keys.sort_unstable();
    across_keys.dedup();
    assert!(across_keys.len() >= 60, "{} distinct ISNs over 64 keys", across_keys.len());
    let across_ports: Vec<u32> = (0..64).map(|p| isn(&key(0), &tuple(49152, 1000 + p), now).get()).collect();
    let mut distinct = across_ports.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(distinct.len() >= 60, "{} distinct ISNs over 64 ports", distinct.len());
    let steps: Vec<u32> = across_ports.windows(2).map(|w| w[1].wrapping_sub(w[0])).collect();
    assert!(steps.windows(2).any(|w| w[0] != w[1]), "the ISN steps by a constant with the port");
    let mut distinct: Vec<u32> = (0..64).map(|p| syn(now, tuple(49152, 2000 + p)).seq).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(distinct.len() >= 60, "the stack's SYNs: {} distinct ISNs over 64 ports", distinct.len());
}

#[test]
fn s_isn_008_timestamp_offset() {
    assert_eq!(ts_offset(&key(0x10), &tuple(49152, 80)), 0xc5ef_61a0);
    let out = syn(Instant::from_millis(5), tuple(49152, 80));
    assert_eq!(out.ts, Some((0xc5ef_61a5, 0)));
}

#[test]
fn s_isn_009_incarnations_share_the_offset() {
    let first = syn(Instant::from_millis(10_000), tuple(49152, 80));
    let second = syn(Instant::from_millis(80_000), tuple(49152, 80));
    assert_eq!(second.ts.unwrap().0.wrapping_sub(first.ts.unwrap().0), 70_000);
}
