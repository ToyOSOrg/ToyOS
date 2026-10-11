//! The SCSI half of a Bulk-Only disk: the commands sent (SPC-4, SBC-3), what
//! each answer means, and the bring-up between a configured interface and a
//! disk with a size — as decisions with no transfer in them.
//!
//! One logical unit, READ(10)/WRITE(10) only, no MODE SENSE. **Everything a
//! device answers is checked and never believed**, and a refusal is by name: a
//! size this driver cannot address, a block size that does not divide the host
//! block, a peripheral that is not a disk.

use crate::Nanos;

/// The block size everything above this driver is written in; a device whose
/// sectors do not divide it is refused, not approximated.
pub const HOST_BLOCK: u32 = 4096;

/// READ(10)'s operation code (SBC-3 §5.11).
pub const READ_10: u8 = 0x28;
/// WRITE(10)'s operation code (SBC-3 §5.32).
pub const WRITE_10: u8 = 0x2A;
/// INQUIRY's operation code (SPC-4 §6.4).
pub const INQUIRY: u8 = 0x12;

/// One command descriptor block, with the direction of the data it moves.
///
/// Built only here, so no command can name a length a CBW cannot carry or a
/// direction that is not its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cdb {
    bytes: [u8; 16],
    len: u8,
    data_in: bool,
}

impl Cdb {
    const fn of<const N: usize>(cdb: [u8; N], data_in: bool) -> Self {
        let mut bytes = [0u8; 16];
        let mut at = 0;
        while at < N {
            bytes[at] = cdb[at];
            at += 1;
        }
        Self { bytes, len: N as u8, data_in }
    }

    /// No data phase (SPC-4 §6.37).
    pub const TEST_UNIT_READY: Self = Self::of([0x00; 6], false);
    /// Fixed-format sense, 18 bytes allocated (SPC-4 §6.29).
    pub const REQUEST_SENSE: Self = Self::of([0x03, 0, 0, 0, SENSE_BYTES as u8, 0], true);
    /// The whole medium, LBA 0 and count 0 — all a block-device flush can mean
    /// (SBC-3 §5.24).
    pub const SYNCHRONIZE_CACHE: Self = Self::of([0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0], false);

    /// READ(10) or WRITE(10) of `sectors` at `lba`.
    pub fn transfer(write: bool, lba: u32, sectors: u16) -> Self {
        let [a, b, c, d] = lba.to_be_bytes();
        let [hi, lo] = sectors.to_be_bytes();
        let opcode = if write { WRITE_10 } else { READ_10 };
        Self::of([opcode, 0, a, b, c, d, 0, hi, lo, 0], !write)
    }

    /// The bytes that go into the CBW.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    pub fn opcode(&self) -> u8 {
        self.bytes[0]
    }

    /// Whether the data phase, if the command has one, is device to host.
    pub fn data_in(&self) -> bool {
        self.data_in
    }
}

/// Bytes a REQUEST SENSE asks for.
pub const SENSE_BYTES: usize = 18;

/// The sense key, ASC and ASCQ of fixed-format sense data (SPC-4 §4.5.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sense {
    pub key: u8,
    pub asc: u8,
    pub ascq: u8,
}

impl Sense {
    /// What a device that would not say is taken to have said: zero is the
    /// failing side of every decision made from it.
    pub const NONE: Self = Self { key: 0, asc: 0, ascq: 0 };

    /// The sense `response` carries, of which `delivered` bytes arrived. ASCQ
    /// is byte 13, so fourteen must have or none of it is believed.
    pub fn of(response: &[u8; SENSE_BYTES], delivered: u32) -> Self {
        if delivered < 14 {
            return Self::NONE;
        }
        Self { key: response[2] & 0x0F, asc: response[12], ascq: response[13] }
    }

    /// ILLEGAL REQUEST / INVALID COMMAND OPERATION CODE: an answer, not a
    /// failure, for a command SBC makes optional.
    pub fn unimplemented(self) -> bool {
        (self.key, self.asc, self.ascq) == (0x05, 0x20, 0x00)
    }
}

impl core::fmt::Display for Sense {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#04x}/{:#04x}/{:#04x}", self.key, self.asc, self.ascq)
    }
}

/// One SCSI command, after the transport's own recovery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reply {
    Ok { delivered: u32 },
    /// Understood and declined: an optional command's caller must tell "I will
    /// not" from "I cannot".
    Refused(Sense),
    /// The transport broke, or the device contradicted itself; nothing about
    /// the buffer is known.
    Broken,
    /// Not issued: the caller's budget was spent. Not a fact about the disk.
    Budget,
}

/// Why an operation failed, for the caller above the disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fail {
    Device,
    /// Nothing is known to have reached the device: ask again.
    Budget,
}

/// What a disk is, from its READ CAPACITY. Only the bring-up sizes one, so
/// every sector it addresses fits READ(10).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    sector_bytes: u32,
    sectors: u64,
    sectors_per_block: u32,
    blocks: u64,
}

impl Geometry {
    /// A disk not yet asked its size, which no transfer fits.
    pub const NONE: Self = Self { sector_bytes: 0, sectors: 0, sectors_per_block: 0, blocks: 0 };

    pub fn sector_bytes(&self) -> u32 {
        self.sector_bytes
    }

    pub fn sectors(&self) -> u64 {
        self.sectors
    }

    /// Whole [`HOST_BLOCK`]s.
    pub fn blocks(&self) -> u64 {
        self.blocks
    }
}

/// A caller's transfer that runs past the disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PastTheEnd {
    pub lba: u64,
    pub count: u32,
    pub blocks: u64,
}

/// `count` host blocks at `lba`, as the READ(10) or WRITE(10) commands that
/// move them, at most `most` blocks each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transfer {
    lba: u64,
    count: u32,
    done: u32,
    write: bool,
    sectors_per_block: u32,
    most: u32,
}

/// One command of a [`Transfer`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Batch {
    pub cdb: Cdb,
    /// Host blocks it moves, and the bytes.
    pub blocks: u32,
    pub bytes: usize,
    /// Where in the caller's buffer they are.
    pub offset: usize,
    /// The host block it starts at.
    pub block: u64,
}

/// What one [`Batch`] came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Moved {
    Whole,
    /// Short of what was asked: nothing above can say which blocks arrived,
    /// so the transfer failed.
    Short { delivered: u32 },
    Refused(Sense),
    Ended(Fail),
}

impl Moved {
    /// Whether the device reported the batch complete, whole or in part.
    pub fn reported(self) -> bool {
        matches!(self, Self::Whole | Self::Short { .. })
    }
}

impl Transfer {
    /// A transfer on a disk of `geometry`; nothing to do for `count` zero.
    pub fn new(lba: u64, count: u32, write: bool, geometry: &Geometry, most: u32) -> Result<Self, PastTheEnd> {
        let sectors = u64::from(most) * u64::from(geometry.sectors_per_block);
        assert!(most > 0 && u16::try_from(sectors).is_ok(), "a batch READ(10) cannot count");
        let fits = lba.checked_add(u64::from(count)).is_some_and(|end| end <= geometry.blocks);
        if count > 0 && !fits {
            return Err(PastTheEnd { lba, count, blocks: geometry.blocks });
        }
        Ok(Self { lba, count, done: 0, write, sectors_per_block: geometry.sectors_per_block, most })
    }

    /// The command owed next, or `None` once every block has moved.
    pub fn next(&self) -> Option<Batch> {
        if self.done == self.count {
            return None;
        }
        let blocks = (self.count - self.done).min(self.most);
        let block = self.lba + u64::from(self.done);
        let sector = block * u64::from(self.sectors_per_block);
        // `BringUp` refused a disk whose last sector does not fit 32 bits, and
        // `new` refused a transfer past the disk.
        let sector = u32::try_from(sector).expect("a sector past what READ(10) addresses");
        let sectors = (blocks * self.sectors_per_block) as u16;
        Some(Batch {
            cdb: Cdb::transfer(self.write, sector, sectors),
            blocks,
            bytes: blocks as usize * HOST_BLOCK as usize,
            offset: self.done as usize * HOST_BLOCK as usize,
            block,
        })
    }

    /// What `reply` to `batch` comes to. **Only the first batch may answer
    /// "ask again"**: blocks already moved are on the device with no way to
    /// resume.
    pub fn answered(&mut self, batch: &Batch, reply: Reply) -> Moved {
        let first = self.done == 0;
        match reply {
            Reply::Ok { delivered } if delivered as usize == batch.bytes => {
                self.done += batch.blocks;
                Moved::Whole
            }
            Reply::Ok { delivered } => Moved::Short { delivered },
            Reply::Refused(sense) => Moved::Refused(sense),
            Reply::Budget if first => Moved::Ended(Fail::Budget),
            Reply::Broken | Reply::Budget => Moved::Ended(Fail::Device),
        }
    }
}

/// What a SYNCHRONIZE CACHE came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flushed {
    /// A cache was emptied.
    Emptied,
    /// The device has no cache to empty (INVALID COMMAND OPERATION CODE): its
    /// writes are durable once complete, and a flush reports nothing wrong.
    NoCache,
    Refused(Sense),
    Ended(Fail),
}

pub fn flushed(reply: Reply) -> Flushed {
    match reply {
        Reply::Refused(sense) if sense.unimplemented() => Flushed::NoCache,
        Reply::Ok { .. } => Flushed::Emptied,
        Reply::Refused(sense) => Flushed::Refused(sense),
        Reply::Broken => Flushed::Ended(Fail::Device),
        Reply::Budget => Flushed::Ended(Fail::Budget),
    }
}

/// A question the bring-up reads an answer to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Query {
    Inquiry,
    Capacity10,
    Capacity16,
}

impl Query {
    pub fn cdb(self) -> Cdb {
        match self {
            Self::Inquiry => Cdb::of([INQUIRY, 0, 0, 0, 36, 0], true),
            Self::Capacity10 => Cdb::of([0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], true),
            // SERVICE ACTION IN(16) / READ CAPACITY(16), 32 bytes allocated.
            Self::Capacity16 => Cdb::of([0x9E, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0], true),
        }
    }

    /// The data phase's length: what the CDB allocates.
    pub fn allocation(self) -> usize {
        match self {
            Self::Inquiry => 36,
            Self::Capacity10 => 8,
            Self::Capacity16 => 32,
        }
    }

    /// Bytes that must arrive for the answer to be read at all.
    fn needs(self) -> usize {
        match self {
            Self::Inquiry => 36,
            Self::Capacity10 => 8,
            Self::Capacity16 => 12,
        }
    }

    pub fn named(self) -> &'static str {
        match self {
            Self::Inquiry => "INQUIRY",
            Self::Capacity10 => "READ CAPACITY(10)",
            Self::Capacity16 => "READ CAPACITY(16)",
        }
    }
}

/// INQUIRY's vendor, product and revision (SPC-4 §6.4.2), bytes 8 to 35.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Inquiry(pub [u8; 28]);

impl Inquiry {
    pub fn vendor(&self) -> &[u8] {
        &self.0[..8]
    }

    pub fn product(&self) -> &[u8] {
        &self.0[8..24]
    }
}

/// What the bring-up asks the driver for next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ask {
    /// A TEST UNIT READY, on the transport directly: NOT READY is expected and
    /// is not a failure to recover from.
    TestUnitReady,
    RequestSense,
    /// Climb the recovery ladder for the TEST UNIT READY that broke.
    Recover,
    /// This question, with the transport's recovery behind it.
    Read(Query),
}

/// What the driver heard back from the last [`Ask`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Heard<'a> {
    /// CSW status 0.
    Good,
    /// CSW status 1: the device holds sense for whoever asks next.
    CheckCondition,
    /// The round trip broke.
    Broke,
    Sense(Sense),
    /// `offline`: the ladder took the device offline.
    Recovered { offline: bool },
    /// The allocation's bytes and how many of them arrived.
    Data { bytes: &'a [u8], delivered: u32 },
    /// Refused, broken, or not issued.
    Unanswered,
}

/// How a bring-up ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Up {
    Ready(Geometry),
    /// It never said it was ready: the sense it last gave, and whether the
    /// ladder took it offline meanwhile. One that is not offline is a device a
    /// later enumeration may find ready.
    Unready { sense: Sense, offline: bool },
    Refused(Refusal),
}

/// Why a device that answered is not a disk this driver serves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    Unanswered(Query),
    /// INQUIRY's peripheral device type, not 0 (direct access).
    NotADisk(u8),
    SectorSize(u32),
    /// A last LBA past what READ(10)'s 32 bits address: serving the first
    /// 2 TiB of a bigger disk would silently truncate it.
    PastRead10 { last_lba: u64 },
    LessThanABlock { sectors: u64, sector_bytes: u32 },
}

/// Where a bring-up stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum At {
    Ready,
    Sense,
    Recovering,
    Read(Query),
}

/// TEST UNIT READY on a budget, then INQUIRY, then READ CAPACITY(10) and (16)
/// where the 10-byte form cannot say: everything between a configured
/// interface and a disk with a size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BringUp {
    at: At,
    /// When no further TEST UNIT READY is started; the one running finishes.
    give_up: Nanos,
    /// The last sense the device gave while not ready.
    sense: Sense,
}

/// Where a bring-up goes after an answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next {
    Ask(BringUp, Ask),
    /// INQUIRY named a direct-access device: this is what it said, and this is
    /// asked next.
    Disk(BringUp, Inquiry, Ask),
    Up(Up),
}

impl BringUp {
    /// A bring-up that stops starting ready attempts at `give_up`.
    pub fn begin(give_up: Nanos) -> (Self, Ask) {
        (Self { at: At::Ready, give_up, sense: Sense::NONE }, Ask::TestUnitReady)
    }

    fn ask(self, at: At, ask: Ask) -> Next {
        Next::Ask(Self { at, ..self }, ask)
    }

    /// Another attempt, if the budget has room for one at `now`.
    fn again(self, now: Nanos, offline: bool) -> Next {
        if offline || now >= self.give_up {
            return Next::Up(Up::Unready { sense: self.sense, offline });
        }
        self.ask(At::Ready, Ask::TestUnitReady)
    }

    /// The last ask was answered with `heard`, at `now`.
    pub fn heard(mut self, heard: Heard<'_>, now: Nanos) -> Next {
        match (self.at, heard) {
            (At::Ready, Heard::Good) => self.ask(At::Read(Query::Inquiry), Ask::Read(Query::Inquiry)),
            // Fetching the sense also clears the condition on a device still
            // spinning up.
            (At::Ready, Heard::CheckCondition) => self.ask(At::Sense, Ask::RequestSense),
            (At::Ready, Heard::Broke) => self.ask(At::Recovering, Ask::Recover),
            (At::Sense, Heard::Sense(sense)) => {
                self.sense = sense;
                self.again(now, false)
            }
            (At::Recovering, Heard::Recovered { offline }) => self.again(now, offline),
            (At::Read(query), Heard::Unanswered) => Next::Up(Up::Refused(Refusal::Unanswered(query))),
            (At::Read(query), Heard::Data { bytes, delivered }) => {
                assert_eq!(bytes.len(), query.allocation(), "the allocation is what is handed back");
                if (delivered as usize) < query.needs() {
                    return Next::Up(Up::Refused(Refusal::Unanswered(query)));
                }
                self.read(query, bytes)
            }
            (at, heard) => panic!("bring-up at {at:?} was answered {heard:?}, which it did not ask for"),
        }
    }

    fn read(self, query: Query, bytes: &[u8]) -> Next {
        let be32 = |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        match query {
            Query::Inquiry => {
                let peripheral = bytes[0] & 0x1F;
                if peripheral != 0 {
                    return Next::Up(Up::Refused(Refusal::NotADisk(peripheral)));
                }
                let mut said = [0u8; 28];
                said.copy_from_slice(&bytes[8..36]);
                let next = Query::Capacity10;
                Next::Disk(Self { at: At::Read(next), ..self }, Inquiry(said), Ask::Read(next))
            }
            // An all-ones last LBA says the disk needs the 16-byte form.
            Query::Capacity10 if be32(0) == u32::MAX => {
                self.ask(At::Read(Query::Capacity16), Ask::Read(Query::Capacity16))
            }
            Query::Capacity10 => Next::Up(geometry(u64::from(be32(0)), be32(4))),
            Query::Capacity16 => {
                let last = (u64::from(be32(0)) << 32) | u64::from(be32(4));
                Next::Up(geometry(last, be32(8)))
            }
        }
    }
}

/// The disk a READ CAPACITY describes, or why it is not one this driver serves.
fn geometry(last_lba: u64, sector_bytes: u32) -> Up {
    // This driver's set and not SBC-3's, which allows any length: 256 divides
    // the host block and is refused.
    if !matches!(sector_bytes, 512 | 1024 | 2048 | 4096) {
        return Up::Refused(Refusal::SectorSize(sector_bytes));
    }
    if last_lba > u64::from(u32::MAX) {
        return Up::Refused(Refusal::PastRead10 { last_lba });
    }
    let sectors = last_lba + 1;
    let sectors_per_block = HOST_BLOCK / sector_bytes;
    let blocks = sectors / u64::from(sectors_per_block);
    if blocks == 0 {
        return Up::Refused(Refusal::LessThanABlock { sectors, sector_bytes });
    }
    Up::Ready(Geometry { sector_bytes, sectors, sectors_per_block, blocks })
}

/// A device-supplied ASCII field, rendered without letting it choose what the
/// log looks like.
pub struct Printable<'a>(pub &'a [u8]);

impl core::fmt::Display for Printable<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("\"")?;
        let mut utf8 = [0u8; 4];
        for &b in self.0 {
            let c = if (0x20..0x7F).contains(&b) && b != b'"' { b as char } else { '.' };
            f.write_str(c.encode_utf8(&mut utf8))?;
        }
        f.write_str("\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Nanos = 1_000_000;

    /// SBC-3 §5.11 and §5.32: the operation code, the LBA big endian in bytes
    /// 2 to 5, the transfer length big endian in 7 and 8.
    #[test]
    fn a_transfer_cdb_is_laid_out_as_sbc_defines_it() {
        let read = Cdb::transfer(false, 0x0102_0304, 0x0506);
        assert_eq!(read.bytes(), [0x28, 0, 1, 2, 3, 4, 0, 5, 6, 0]);
        assert!(read.data_in());
        let write = Cdb::transfer(true, 0xFFFF_FFFF, 1);
        assert_eq!(write.bytes(), [0x2A, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 1, 0]);
        assert!(!write.data_in());
    }

    #[test]
    fn every_fixed_cdb_carries_its_own_length_direction_and_allocation() {
        assert_eq!(Cdb::TEST_UNIT_READY.bytes(), [0; 6]);
        assert_eq!(Cdb::REQUEST_SENSE.bytes(), [0x03, 0, 0, 0, 18, 0]);
        assert_eq!(Cdb::SYNCHRONIZE_CACHE.bytes(), [0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(!Cdb::TEST_UNIT_READY.data_in() && !Cdb::SYNCHRONIZE_CACHE.data_in());
        assert!(Cdb::REQUEST_SENSE.data_in());
        assert_eq!(Query::Inquiry.cdb().bytes(), [0x12, 0, 0, 0, 36, 0]);
        assert_eq!(Query::Capacity10.cdb().bytes(), [0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(Query::Capacity16.cdb().bytes(), [0x9E, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0]);
        for query in [Query::Inquiry, Query::Capacity10, Query::Capacity16] {
            assert!(query.cdb().data_in(), "{query:?}");
            assert!(query.needs() <= query.allocation(), "{query:?}");
        }
        // The allocation length the CDB states is the data phase asked for.
        assert_eq!(usize::from(Query::Inquiry.cdb().bytes()[4]), Query::Inquiry.allocation());
        assert_eq!(usize::from(Query::Capacity16.cdb().bytes()[13]), Query::Capacity16.allocation());
    }

    #[test]
    fn sense_is_believed_only_when_its_ascq_arrived() {
        let mut response = [0u8; SENSE_BYTES];
        response[2] = 0xF5;
        response[12] = 0x20;
        response[13] = 0x00;
        assert_eq!(Sense::of(&response, 14), Sense { key: 0x05, asc: 0x20, ascq: 0 });
        assert!(Sense::of(&response, 18).unimplemented());
        assert_eq!(Sense::of(&response, 13), Sense::NONE);
        assert!(!Sense::NONE.unimplemented());
        assert!(!Sense { key: 0x04, asc: 0x44, ascq: 0 }.unimplemented());
        extern crate std;
        use std::string::ToString;
        assert_eq!(Sense { key: 0x04, asc: 0x44, ascq: 0 }.to_string(), "0x04/0x44/0x00");
    }

    const STICK: Geometry = Geometry { sector_bytes: 512, sectors: 160, sectors_per_block: 8, blocks: 20 };

    #[test]
    fn a_transfer_past_the_disk_is_refused_and_an_empty_one_is_nothing() {
        let past = |lba, count| Transfer::new(lba, count, false, &STICK, 8);
        assert_eq!(past(19, 2), Err(PastTheEnd { lba: 19, count: 2, blocks: 20 }));
        assert_eq!(past(u64::MAX, 1).map(|_| ()), Err(PastTheEnd { lba: u64::MAX, count: 1, blocks: 20 }));
        assert!(past(19, 1).is_ok());
        let empty = past(u64::MAX, 0).expect("nothing to move");
        assert_eq!(empty.next(), None);
        let no_size = Transfer::new(0, 1, false, &Geometry::NONE, 8);
        assert_eq!(no_size, Err(PastTheEnd { lba: 0, count: 1, blocks: 0 }));
        assert_eq!(Transfer::new(0, 0, false, &Geometry::NONE, 8).expect("nothing to move").next(), None);
    }

    /// Batches of at most `most` blocks, each at its own sector and offset,
    /// and a batch that did not move whole is not moved past.
    #[test]
    fn a_transfer_is_its_batches_in_order_and_only_a_whole_one_advances() {
        let mut transfer = Transfer::new(2, 18, true, &STICK, 8).expect("fits");
        let mut seen = [None; 4];
        for slot in &mut seen {
            let Some(batch) = transfer.next() else { break };
            *slot = Some((batch.cdb, batch.blocks, batch.offset, batch.block));
            let bytes = batch.bytes as u32;
            assert_eq!(transfer.answered(&batch, Reply::Ok { delivered: bytes - 1 }), Moved::Short { delivered: bytes - 1 });
            assert_eq!(transfer.next(), Some(batch), "a short batch is not moved past");
            assert_eq!(transfer.answered(&batch, Reply::Ok { delivered: bytes }), Moved::Whole);
        }
        assert_eq!(seen, [
            Some((Cdb::transfer(true, 16, 64), 8, 0, 2)),
            Some((Cdb::transfer(true, 80, 64), 8, 8 * 4096, 10)),
            Some((Cdb::transfer(true, 144, 16), 2, 16 * 4096, 18)),
            None,
        ]);
    }

    /// "Ask again" says nothing reached the device, which is true only before
    /// the first batch moved.
    #[test]
    fn only_the_first_batch_may_answer_ask_again() {
        let mut transfer = Transfer::new(0, 16, false, &STICK, 8).expect("fits");
        let first = transfer.next().expect("a batch");
        assert_eq!(transfer.answered(&first, Reply::Budget), Moved::Ended(Fail::Budget));
        assert_eq!(transfer.answered(&first, Reply::Broken), Moved::Ended(Fail::Device));
        let sense = Sense { key: 3, asc: 0x11, ascq: 0 };
        assert_eq!(transfer.answered(&first, Reply::Refused(sense)), Moved::Refused(sense));
        assert_eq!(transfer.answered(&first, Reply::Ok { delivered: 8 * 4096 }), Moved::Whole);
        let second = transfer.next().expect("a batch");
        assert_eq!(transfer.answered(&second, Reply::Budget), Moved::Ended(Fail::Device));
        assert!(Moved::Whole.reported() && Moved::Short { delivered: 1 }.reported());
        assert!(!Moved::Refused(sense).reported() && !Moved::Ended(Fail::Budget).reported());
    }

    #[test]
    fn a_flush_the_device_does_not_implement_is_no_failure_and_every_other_refusal_is() {
        let unimplemented = Sense { key: 0x05, asc: 0x20, ascq: 0 };
        let failed = Sense { key: 0x04, asc: 0x44, ascq: 0 };
        assert_eq!(flushed(Reply::Refused(unimplemented)), Flushed::NoCache);
        assert_eq!(flushed(Reply::Refused(failed)), Flushed::Refused(failed));
        assert_eq!(flushed(Reply::Ok { delivered: 0 }), Flushed::Emptied);
        assert_eq!(flushed(Reply::Broken), Flushed::Ended(Fail::Device));
        assert_eq!(flushed(Reply::Budget), Flushed::Ended(Fail::Budget));
    }

    fn inquiry(peripheral: u8) -> [u8; 36] {
        let mut data = [0u8; 36];
        data[0] = peripheral;
        data[8..16].copy_from_slice(b"QEMU    ");
        data[16..32].copy_from_slice(b"QEMU HARDDISK   ");
        data[32..36].copy_from_slice(b"2.5+");
        data
    }

    fn capacity10(last: u32, bytes: u32) -> [u8; 8] {
        let mut data = [0u8; 8];
        data[..4].copy_from_slice(&last.to_be_bytes());
        data[4..].copy_from_slice(&bytes.to_be_bytes());
        data
    }

    fn capacity16(last: u64, bytes: u32) -> [u8; 32] {
        let mut data = [0u8; 32];
        data[..8].copy_from_slice(&last.to_be_bytes());
        data[8..12].copy_from_slice(&bytes.to_be_bytes());
        data
    }

    /// A device that answers every ask as `answer` says; `None` is the ask
    /// the test expects never to be made.
    struct Device<'a> {
        not_ready: u32,
        breaks: u32,
        offline: bool,
        inquiry: Option<&'a [u8]>,
        capacity10: Option<&'a [u8]>,
        capacity16: Option<&'a [u8]>,
    }

    const LONGEST: usize = 16;

    struct Route {
        asks: [Option<Ask>; LONGEST],
        disk: Option<Inquiry>,
        up: Up,
    }

    impl Route {
        fn asks(&self) -> impl Iterator<Item = Ask> + '_ {
            self.asks.iter().map_while(|a| *a)
        }
    }

    /// A bring-up driven to its end against `device`, the clock advancing
    /// `step` per ask from 0 with a 500 ms budget.
    fn bring_up(mut device: Device<'_>, step: Nanos) -> Route {
        let (mut up, mut ask) = BringUp::begin(500 * MS);
        let mut asks = [None; LONGEST];
        let mut disk = None;
        let mut now = 0;
        for slot in &mut asks {
            *slot = Some(ask);
            now += step;
            let heard = match ask {
                Ask::TestUnitReady if device.breaks > 0 => {
                    device.breaks -= 1;
                    Heard::Broke
                }
                Ask::TestUnitReady if device.not_ready > 0 => {
                    device.not_ready -= 1;
                    Heard::CheckCondition
                }
                Ask::TestUnitReady => Heard::Good,
                Ask::RequestSense => Heard::Sense(Sense { key: 0x02, asc: 0x04, ascq: 0x01 }),
                Ask::Recover => Heard::Recovered { offline: device.offline },
                Ask::Read(query) => {
                    let data = match query {
                        Query::Inquiry => device.inquiry,
                        Query::Capacity10 => device.capacity10,
                        Query::Capacity16 => device.capacity16,
                    };
                    match data {
                        Some(bytes) => Heard::Data { bytes, delivered: bytes.len() as u32 },
                        None => Heard::Unanswered,
                    }
                }
            };
            match up.heard(heard, now) {
                Next::Ask(next, then) => (up, ask) = (next, then),
                Next::Disk(next, said, then) => {
                    disk = Some(said);
                    (up, ask) = (next, then);
                }
                Next::Up(end) => return Route { asks, disk, up: end },
            }
        }
        panic!("a bring-up that does not end: {asks:?}");
    }

    fn disk<'a>(inquiry: &'a [u8], capacity10: &'a [u8]) -> Device<'a> {
        Device {
            not_ready: 0,
            breaks: 0,
            offline: false,
            inquiry: Some(inquiry),
            capacity10: Some(capacity10),
            capacity16: None,
        }
    }

    #[test]
    fn a_ready_disk_is_asked_what_it_is_and_how_big_and_nothing_else() {
        let (inquiry, capacity) = (inquiry(0), capacity10(8191, 512));
        let route = bring_up(disk(&inquiry, &capacity), MS);
        assert!(route.asks().eq([Ask::TestUnitReady, Ask::Read(Query::Inquiry), Ask::Read(Query::Capacity10)]));
        assert_eq!(route.up, Up::Ready(Geometry { sector_bytes: 512, sectors: 8192, sectors_per_block: 8, blocks: 1024 }));
        let said = route.disk.expect("INQUIRY said a disk");
        assert_eq!((said.vendor(), said.product()), (&b"QEMU    "[..], &b"QEMU HARDDISK   "[..]));
        assert_eq!(&said.0[24..], b"2.5+");
    }

    /// Each NOT READY is followed by the sense that clears it, and the device
    /// is asked again until it is ready.
    #[test]
    fn a_disk_spinning_up_is_asked_for_its_sense_and_then_again() {
        let (inquiry, capacity) = (inquiry(0), capacity10(8191, 4096));
        let route = bring_up(Device { not_ready: 2, ..disk(&inquiry, &capacity) }, MS);
        assert!(route.asks().take(5).eq([
            Ask::TestUnitReady,
            Ask::RequestSense,
            Ask::TestUnitReady,
            Ask::RequestSense,
            Ask::TestUnitReady,
        ]));
        assert!(matches!(route.up, Up::Ready(Geometry { sectors_per_block: 1, blocks: 8192, .. })));
    }

    /// The budget bounds when attempts stop being *started*: the sense of the
    /// last one is what the refusal says.
    #[test]
    fn a_disk_that_never_becomes_ready_is_given_up_on_at_its_budget() {
        let (inquiry, capacity) = (inquiry(0), capacity10(8191, 512));
        let route = bring_up(Device { not_ready: u32::MAX, ..disk(&inquiry, &capacity) }, 100 * MS);
        assert_eq!(route.up, Up::Unready { sense: Sense { key: 0x02, asc: 0x04, ascq: 0x01 }, offline: false });
        assert_eq!(route.asks().filter(|a| *a == Ask::TestUnitReady).count(), 3, "at 0, 200 and 400 ms");
        assert_eq!(route.disk, None);
    }

    #[test]
    fn a_ready_attempt_that_breaks_is_recovered_and_one_taken_offline_ends_it() {
        let (inquiry, capacity) = (inquiry(0), capacity10(8191, 512));
        let recovered = bring_up(Device { breaks: 1, ..disk(&inquiry, &capacity) }, MS);
        assert!(recovered.asks().take(3).eq([Ask::TestUnitReady, Ask::Recover, Ask::TestUnitReady]));
        assert!(matches!(recovered.up, Up::Ready(_)));
        let offline = bring_up(Device { breaks: 1, offline: true, ..disk(&inquiry, &capacity) }, MS);
        assert!(offline.asks().eq([Ask::TestUnitReady, Ask::Recover]));
        assert_eq!(offline.up, Up::Unready { sense: Sense::NONE, offline: true });
    }

    /// READ CAPACITY(10) says all ones when the disk needs sixteen bytes to
    /// describe it, and only then is the 16-byte form asked.
    #[test]
    fn a_disk_too_big_for_read_capacity_10_is_asked_the_16_byte_form() {
        let (inquiry, ten) = (inquiry(0), capacity10(u32::MAX, 512));
        let sixteen = capacity16(u64::from(u32::MAX), 4096);
        let route = bring_up(Device { capacity16: Some(&sixteen), ..disk(&inquiry, &ten) }, MS);
        assert_eq!(route.asks().last(), Some(Ask::Read(Query::Capacity16)));
        assert_eq!(
            route.up,
            Up::Ready(Geometry { sector_bytes: 4096, sectors: 1 << 32, sectors_per_block: 1, blocks: 1 << 32 })
        );
        let past = capacity16(u64::from(u32::MAX) + 1, 512);
        let route = bring_up(Device { capacity16: Some(&past), ..disk(&inquiry, &ten) }, MS);
        assert_eq!(route.up, Up::Refused(Refusal::PastRead10 { last_lba: 1 << 32 }));
    }

    #[test]
    fn a_device_that_is_not_a_disk_this_driver_serves_is_refused_by_name() {
        let (disk_inquiry, fine) = (inquiry(0), capacity10(8191, 512));
        let cdrom = inquiry(0x05);
        assert_eq!(bring_up(disk(&cdrom, &fine), MS).up, Up::Refused(Refusal::NotADisk(0x05)));
        // The qualifier bits are not the type.
        assert!(matches!(bring_up(disk(&inquiry(0x20), &fine), MS).up, Up::Ready(_)));
        for bytes in [0, 520, 8192] {
            let odd = capacity10(8191, bytes);
            assert_eq!(bring_up(disk(&disk_inquiry, &odd), MS).up, Up::Refused(Refusal::SectorSize(bytes)));
        }
        // No oracle but the driver's own set: SBC-3 allows 256, which divides
        // the host block.
        let small = capacity10(8191, 256);
        assert_eq!(bring_up(disk(&disk_inquiry, &small), MS).up, Up::Refused(Refusal::SectorSize(256)));
        let tiny = capacity10(6, 512);
        assert_eq!(
            bring_up(disk(&disk_inquiry, &tiny), MS).up,
            Up::Refused(Refusal::LessThanABlock { sectors: 7, sector_bytes: 512 })
        );
    }

    #[test]
    fn a_question_unanswered_or_answered_short_refuses_the_device() {
        let (whole, fine) = (inquiry(0), capacity10(8191, 512));
        let silent = Device { inquiry: None, ..disk(&whole, &fine) };
        assert_eq!(bring_up(silent, MS).up, Up::Refused(Refusal::Unanswered(Query::Inquiry)));
        let no_size = Device { capacity10: None, ..disk(&whole, &fine) };
        assert_eq!(bring_up(no_size, MS).up, Up::Refused(Refusal::Unanswered(Query::Capacity10)));

        let (up, _) = BringUp::begin(500 * MS);
        let Next::Ask(up, _) = up.heard(Heard::Good, 0) else { panic!("INQUIRY is asked") };
        let short = up.heard(Heard::Data { bytes: &whole, delivered: 35 }, 0);
        assert_eq!(short, Next::Up(Up::Refused(Refusal::Unanswered(Query::Inquiry))));
    }

    #[test]
    #[should_panic(expected = "which it did not ask for")]
    fn an_answer_to_something_not_asked_is_a_driver_bug() {
        let (up, _) = BringUp::begin(500 * MS);
        let _ = up.heard(Heard::Sense(Sense::NONE), 0);
    }

    #[test]
    fn a_device_field_prints_as_the_characters_it_carries_and_nothing_else() {
        extern crate std;
        use std::string::ToString;
        assert_eq!(Printable(b"AB\"1\x7f\n z").to_string(), "\"AB.1.. z\"");
    }
}
