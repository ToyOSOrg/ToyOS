//! Writes a bench's scratch disk through its USB driver until the machine
//! resets beneath it: the one partition its `write:scratch` authorization
//! lists, from its start and wrapping, with the session's arena kept full so a
//! write is always on the wire, and a flush once each half mebibyte is
//! acknowledged.
//!
//! With no argument it writes for ever, so the boot deadline resets the
//! machine (`scratch_written_through_a_deadline_reset`). With `reboot` it asks
//! the supervisor for a reboot once [`REBOOT_AFTER_MIB`] are durable and goes
//! on writing through the stop (`scratch_written_through_a_reboot`).

use std::collections::BTreeMap;

use diskserver::{Outcome, Session, Ticket, Unsent};
use toyos::power::{stop, Mode, Stop};
use toyos_blockring::{BLOCK_BYTES, MAX_REQUEST_BLOCKS};
use toyos_manifest::authorize::Authorization;

/// The mebibytes durable before the reboot is asked for: a few seconds of
/// either bench's stick.
const REBOOT_AFTER_MIB: u64 = 64;

const MIB: u64 = 1 << 20;
const REQUEST_BYTES: u64 = MAX_REQUEST_BLOCKS as u64 * BLOCK_BYTES as u64;
/// Acknowledged writes a flush is asked for after. A write stays in the
/// arena until a flush covers it, and a flush covers only what was
/// acknowledged before it went out.
const FLUSH_AFTER: u64 = MIB / 2 / REQUEST_BYTES;

fn main() {
    let reboot = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("reboot") => true,
        Some(other) => panic!("scratch_load: `{other}` is no argument; `reboot` is the one there is"),
    };
    let service = Authorization::WriteScratch.name();
    let endowed = toyos::endow::namespace().expect("scratch_load: no namespace");
    let names = toyos::namespace::build()
        .keep(endowed, &[service])
        .finish()
        .unwrap_or_else(|_| panic!("scratch_load: this run holds no `{service}`"));
    let listed = diskserver::list(&names, service).unwrap_or_else(|why| panic!("scratch_load: no scratch partition is listed: {why:?}"));
    let [one] = listed[..] else {
        panic!("scratch_load: {} scratch partitions are listed, and the load writes one", listed.len());
    };
    let mut session = Session::open(names, service, one.unique)
        .unwrap_or_else(|why| panic!("scratch_load: the scratch partition on disk {} would not open: {why:?}", one.disk));
    let span = session.blocks() / u64::from(MAX_REQUEST_BLOCKS) * u64::from(MAX_REQUEST_BLOCKS);
    assert!(span * BLOCK_BYTES as u64 >= MIB, "scratch_load: the scratch partition is under a mebibyte");
    println!("scratch_load: writing disk {}'s scratch partition, {} MiB", one.disk, span * BLOCK_BYTES as u64 / MIB);

    let data: Vec<u8> = (0..REQUEST_BYTES).map(|i| (i * 31 + 7) as u8).collect();
    // Each flush on the wire, and the writes it covers.
    let mut flushes: BTreeMap<Ticket, u64> = BTreeMap::new();
    let (mut lba, mut acked, mut durable, mut asked) = (0u64, 0u64, 0u64, false);
    loop {
        loop {
            match session.submit_write(lba, &data) {
                Ok(_) => lba = (lba + u64::from(MAX_REQUEST_BLOCKS)) % span,
                Err(Unsent::ArenaFull) => break,
                Err(Unsent::Ended) => panic!("scratch_load: the block service ended the session"),
            }
        }
        let waited = session.wait(true);
        if waited.ended && asked {
            // The stop's own doing: the driver stops before the machine does.
            println!("scratch_load: the stop ended the session with {} MiB durable", durable / MIB);
            loop {
                std::thread::park();
            }
        }
        assert!(!waited.ended, "scratch_load: the block service ended the session with {} MiB durable", durable / MIB);
        for answer in waited.answers {
            match (flushes.remove(&answer.ticket), answer.outcome) {
                (None, Outcome::Done) => acked += 1,
                (Some(writes), Outcome::Durable) => {
                    let before = durable / MIB;
                    durable += writes * REQUEST_BYTES;
                    if durable / MIB / 16 > before / 16 {
                        println!("scratch_load: {} MiB durable", durable / MIB);
                    }
                }
                // The stop's own doing, once it is asked for: usbd ends the
                // device's commands before the machine stops.
                (_, other) if asked => println!("scratch_load: in the stop a request was answered {other:?}"),
                (_, other) => panic!("scratch_load: a request was answered {other:?} with {} MiB durable", durable / MIB),
            }
        }
        if acked >= FLUSH_AFTER {
            let ticket = session.submit_flush().expect("scratch_load: a flush is never refused while the session is up");
            flushes.insert(ticket, acked);
            acked = 0;
        }
        if reboot && !asked && durable >= REBOOT_AFTER_MIB * MIB {
            asked = true;
            if let Err(why) = stop(Stop::Reboot, Mode::Terminate) {
                panic!("scratch_load: the reboot was refused: {why:?}");
            }
            println!("scratch_load: the reboot is asked for with {} MiB durable, and the writes go on", durable / MIB);
        }
    }
}
