//! A designation's steps, apart from the device and memory they run on: read
//! the front of the partition, judge it ([`crate::content`]), and only for a
//! partition it does not recognise and a request that writes, write the stamp
//! as one whole block built in memory, flush, read it back and compare.
//!
//! The caller issues each [`Ask`] on its device and reports each command's
//! answer; one command is on the device at a time.

use toyos_blockring::wire::{Designated, Refusal};
use toyos_blockring::{BLOCK_BYTES, MAX_REQUEST_BLOCKS};

use crate::content::{self, Content};

/// Where in the job's memory each piece is: the front read, the stamp, and
/// the stamp read back, after a first block the job does not use.
pub const FRONT: usize = BLOCK_BYTES;
pub const STAMP: usize = FRONT + content::READ_BYTES;
pub const BACK: usize = STAMP + BLOCK_BYTES;
/// The memory a job needs: a session's region holds it.
pub const BYTES: usize = BACK + BLOCK_BYTES;
const _: () = assert!(BYTES <= toyos_blockring::SESSION_BYTES);

/// The job's memory, which the device moves blocks into and out of.
pub trait Memory {
    fn copy_out(&self, at: usize, out: &mut [u8]);
    fn copy_in(&mut self, at: usize, data: &[u8]);
}

/// What the job asks of its caller next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing until a command is answered, or until the device has room.
    Wait,
    /// Read `count` blocks from the partition's block `block` into memory at `at`.
    Read { block: u64, count: u32, at: usize },
    /// Write the block in memory at `at` to the partition's block `block`.
    Write { block: u64, at: usize },
    /// Flush the device's volatile cache; a device with none is answered at once.
    Flush,
    Done(Result<Designated, Refusal>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Read { next: u64, end: u64 },
    Write,
    Flush,
    Back,
    Compare,
}

pub struct Designation {
    blocks: u64,
    commit: bool,
    phase: Phase,
    /// A command is on the device.
    issued: bool,
    /// A command failed, or the device was reset beneath one.
    failed: bool,
}

impl Designation {
    /// A designation of a partition of `blocks` blocks: one that writes when
    /// `commit`, and judges only otherwise.
    pub fn new(blocks: u64, commit: bool) -> Self {
        let end = blocks.min((content::READ_BYTES / BLOCK_BYTES) as u64);
        Self { blocks, commit, phase: Phase::Read { next: 0, end }, issued: false, failed: false }
    }

    /// The next step, given whether the device has `room` for a command.
    pub fn next(&mut self, memory: &mut dyn Memory, room: bool) -> Ask {
        if self.issued {
            return Ask::Wait;
        }
        if self.failed {
            return Ask::Done(Err(Refusal::Device));
        }
        let needs_device = match self.phase {
            Phase::Read { next, end } => next < end,
            Phase::Compare => false,
            Phase::Write | Phase::Flush | Phase::Back => true,
        };
        if needs_device && !room {
            return Ask::Wait;
        }
        match self.phase {
            Phase::Read { next, end } if next < end => {
                let count = (end - next).min(u64::from(MAX_REQUEST_BLOCKS));
                self.phase = Phase::Read { next: next + count, end };
                self.issue(Ask::Read { block: next, count: count as u32, at: FRONT + next as usize * BLOCK_BYTES })
            }
            Phase::Read { end, .. } => {
                let mut front = vec![0u8; end as usize * BLOCK_BYTES];
                memory.copy_out(FRONT, &mut front);
                match content::judge(&front, self.blocks) {
                    Content::Volume => Ask::Done(Ok(Designated::Volume)),
                    Content::Designated(_) => Ask::Done(Ok(Designated::Already)),
                    Content::Foreign(name) => Ask::Done(Ok(Designated::Foreign(name.into()))),
                    Content::Unrecognised if !self.commit => Ask::Done(Ok(Designated::WouldWrite)),
                    Content::Unrecognised => {
                        memory.copy_in(STAMP, &bcachefs::designation(self.blocks));
                        self.phase = Phase::Write;
                        self.next(memory, room)
                    }
                }
            }
            Phase::Write => {
                self.phase = Phase::Flush;
                self.issue(Ask::Write { block: 0, at: STAMP })
            }
            Phase::Flush => {
                self.phase = Phase::Back;
                self.issue(Ask::Flush)
            }
            Phase::Back => {
                self.phase = Phase::Compare;
                self.issue(Ask::Read { block: 0, count: 1, at: BACK })
            }
            Phase::Compare => {
                let (mut back, mut stamp) = ([0u8; BLOCK_BYTES], [0u8; BLOCK_BYTES]);
                memory.copy_out(BACK, &mut back);
                memory.copy_out(STAMP, &mut stamp);
                Ask::Done(if back == stamp { Ok(Designated::Written) } else { Err(Refusal::Device) })
            }
        }
    }

    fn issue(&mut self, ask: Ask) -> Ask {
        self.issued = true;
        ask
    }

    /// The command on the device was answered.
    pub fn answered(&mut self, ok: bool) {
        assert!(self.issued, "an answer for a designation with nothing on the device");
        self.issued = false;
        self.failed |= !ok;
    }

    /// The device was reset beneath the command on it, which is never answered.
    pub fn reset(&mut self) {
        if self.issued {
            self.issued = false;
            self.failed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Memory for Vec<u8> {
        fn copy_out(&self, at: usize, out: &mut [u8]) {
            out.copy_from_slice(&self[at..at + out.len()]);
        }
        fn copy_in(&mut self, at: usize, data: &[u8]) {
            self[at..at + data.len()].copy_from_slice(data);
        }
    }

    /// A partition of whole blocks, a device with a volatile cache that holds
    /// writes until a flush, and every command the job issued.
    struct Disk {
        blocks: Vec<[u8; BLOCK_BYTES]>,
        cached: Vec<(u64, [u8; BLOCK_BYTES])>,
        cache: bool,
        /// The command number that fails, counted from 0.
        fails: Option<usize>,
        /// A device that answers a write it did not keep.
        drops_writes: bool,
        commands: Vec<Ask>,
    }

    impl Disk {
        fn new(blocks: usize, fill: impl Fn(usize) -> u8) -> Self {
            let blocks = (0..blocks).map(|b| [fill(b); BLOCK_BYTES]).collect();
            Self { blocks, cached: Vec::new(), cache: true, fails: None, drops_writes: false, commands: Vec::new() }
        }

        /// Run `job` to its end, answering each command as the device would.
        fn run(&mut self, job: &mut Designation, memory: &mut Vec<u8>) -> Result<Designated, Refusal> {
            loop {
                let ask = job.next(memory, true);
                let n = self.commands.len();
                self.commands.push(ask.clone());
                let ok = self.fails != Some(n);
                match ask {
                    Ask::Done(done) => return done,
                    Ask::Wait => panic!("a job with nothing on the device waits"),
                    Ask::Read { block, count, at } => {
                        for i in 0..count as usize {
                            let b = block as usize + i;
                            let data = self.cached.iter().rev().find(|(c, _)| *c as usize == b).map_or(self.blocks[b], |(_, d)| *d);
                            memory.copy_in(at + i * BLOCK_BYTES, &data);
                        }
                    }
                    Ask::Write { block, at } => {
                        let mut data = [0u8; BLOCK_BYTES];
                        memory.copy_out(at, &mut data);
                        if !self.drops_writes {
                            if self.cache {
                                self.cached.push((block, data));
                            } else {
                                self.blocks[block as usize] = data;
                            }
                        }
                    }
                    Ask::Flush => {
                        for (b, d) in self.cached.drain(..) {
                            self.blocks[b as usize] = d;
                        }
                    }
                }
                job.answered(ok);
            }
        }
    }

    fn memory() -> Vec<u8> {
        vec![0; BYTES]
    }

    /// An unrecognised partition is stamped on the medium, flushed before it
    /// is read back, and the rest of it is untouched; the front it judged is
    /// the first 1 MiB, in requests no larger than the ring allows.
    #[test]
    fn an_unrecognised_partition_is_stamped_flushed_and_read_back() {
        let mut disk = Disk::new(1000, |b| (b % 251) as u8 | 1);
        let mut job = Designation::new(1000, true);
        assert_eq!(disk.run(&mut job, &mut memory()), Ok(Designated::Written));
        assert!(disk.blocks[0] == bcachefs::designation(1000), "the stamp is on the medium, not in the cache");
        assert!(disk.blocks[1..].iter().enumerate().all(|(i, b)| b[0] == ((i + 1) % 251) as u8 | 1));
        let reads: u64 = disk
            .commands
            .iter()
            .filter_map(|c| match c {
                Ask::Read { count, at, .. } if *at >= FRONT && *at < STAMP => Some(u64::from(*count)),
                _ => None,
            })
            .sum();
        assert_eq!(reads, 256);
        assert!(disk.commands.iter().all(|c| !matches!(c, Ask::Read { count, .. } if *count > MAX_REQUEST_BLOCKS)));
        let order: Vec<&str> = disk
            .commands
            .iter()
            .filter_map(|c| match c {
                Ask::Write { .. } => Some("write"),
                Ask::Flush => Some("flush"),
                Ask::Read { at, .. } if *at == BACK => Some("back"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["write", "flush", "back"]);
    }

    /// What the judge recognises is answered and nothing is written; a judge-only
    /// request writes nothing either.
    #[test]
    fn a_recognised_or_judged_partition_is_never_written() {
        let mut ours = Disk::new(300, |_| 0);
        ours.blocks[0][..4].copy_from_slice(b"BCFS");
        let mut stamped = Disk::new(300, |_| 0);
        stamped.blocks[0] = bcachefs::designation(300);
        let mut ext4 = Disk::new(300, |_| 0);
        ext4.blocks[0][1080..1082].copy_from_slice(&[0x53, 0xEF]);
        let cases = [
            (ours, true, Designated::Volume),
            (stamped, true, Designated::Already),
            (ext4, true, Designated::Foreign("an ext2/3/4 file system".into())),
            (Disk::new(300, |_| 7), false, Designated::WouldWrite),
        ];
        for (mut disk, commit, want) in cases {
            let before = disk.blocks.clone();
            let mut job = Designation::new(300, commit);
            assert_eq!(disk.run(&mut job, &mut memory()), Ok(want.clone()));
            assert!(disk.blocks == before && disk.commands.iter().all(|c| !matches!(c, Ask::Write { .. } | Ask::Flush)), "{want:?}");
        }
    }

    /// A failed command, a device that drops the write it answered, and a
    /// reset beneath a command each end the job refused `Device`, never
    /// `Written`.
    #[test]
    fn a_failed_or_unkept_write_is_never_answered_written() {
        let mut job = Designation::new(300, true);
        let mut probe = Disk::new(300, |_| 3);
        probe.run(&mut job, &mut memory()).unwrap();
        let write = probe.commands.iter().position(|c| matches!(c, Ask::Write { .. })).unwrap();
        for fails in [0, write, write + 1, write + 2] {
            let mut disk = Disk { fails: Some(fails), ..Disk::new(300, |_| 3) };
            assert_eq!(disk.run(&mut Designation::new(300, true), &mut memory()), Err(Refusal::Device), "command {fails} failed");
        }
        let mut dropping = Disk { drops_writes: true, ..Disk::new(300, |_| 3) };
        assert_eq!(dropping.run(&mut Designation::new(300, true), &mut memory()), Err(Refusal::Device));

        let mut job = Designation::new(300, true);
        let mut mem = memory();
        assert!(matches!(job.next(&mut mem, true), Ask::Read { .. }));
        job.reset();
        assert_eq!(job.next(&mut mem, true), Ask::Done(Err(Refusal::Device)));
    }

    /// No room on the device is a wait, never a skipped step; a device with no
    /// cache answers the flush at once and the stamp still reads back.
    #[test]
    fn a_full_device_waits_and_a_cacheless_one_still_reads_back() {
        let mut job = Designation::new(10, true);
        let mut mem = memory();
        assert_eq!(job.next(&mut mem, false), Ask::Wait);
        let mut disk = Disk { cache: false, ..Disk::new(10, |_| 9) };
        assert_eq!(disk.run(&mut job, &mut mem), Ok(Designated::Written));
        assert!(disk.blocks[0] == bcachefs::designation(10), "the stamp is on the medium");
    }
}
