//! A device as a crash leaves it, for checking what a filesystem writes
//! through one.
//!
//! **A completed write is held in a volatile cache until a flush covers it.**
//! A crash keeps every flushed write and, of the writes no flush covered, any
//! subset of their atomic units: nothing orders how a cache reaches the
//! medium, nor the units of one command. So each unit some unflushed write
//! covers holds, after the crash, either what was durable or what any one of
//! those writes carried; a write kept in part is a torn write.
//!
//! **The unit is the device's**, never the format's block: one logical block
//! (512 bytes on a 512-byte-LBA device) unless the device reports a larger
//! unit it writes whole across a power loss.
//!
//! [`Model::check`] judges every medium a crash could leave now when there
//! are at most its budget of them, and that many drawn from a seed otherwise.
//! A [`Violation`] names the seed and the state, so a red run is replayed.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt;

use crate::{within, Block, BlockDev, Error, BLOCK};

/// A device whose every request completes as it is asked.
pub struct Model {
    state: RefCell<State>,
    blocks: u64,
    unit: usize,
}

struct State {
    durable: Vec<u8>,
    /// In the order they completed.
    unflushed: Vec<Unflushed>,
}

struct Unflushed {
    /// The byte the write begins at.
    at: usize,
    data: Vec<u8>,
}

/// How [`Model::check`] reached the states it judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Search {
    /// Every state there is, this many.
    Every(u64),
    /// This many, drawn from the seed: more states than the budget.
    Sampled { seed: u64, samples: u64 },
}

/// A crash state the judge refused.
#[derive(Debug)]
pub struct Violation {
    pub search: Search,
    /// The state's index among every state, or its sample number.
    pub state: u64,
    pub why: String,
    /// The medium the crash left.
    pub image: Vec<u8>,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.search {
            Search::Every(n) => write!(f, "crash state {} of every {n}: {}", self.state, self.why),
            Search::Sampled { seed, samples } => {
                write!(f, "crash sample {} of {samples} from seed {seed:#x}: {}", self.state, self.why)
            }
        }
    }
}

/// One unit some unflushed write covers: where it is, and each write's
/// version of it as `(write, offset in its data)`.
struct Unit {
    at: usize,
    versions: Vec<(usize, usize)>,
}

impl Model {
    /// A device holding `image` durably, whose writes are kept whole in
    /// `unit`-byte pieces.
    pub fn new(image: Vec<u8>, unit: usize) -> Self {
        assert!(unit > 0 && BLOCK.is_multiple_of(unit), "an atomic unit of {unit} bytes does not divide a block");
        assert!(image.len().is_multiple_of(BLOCK), "a device over {} bytes, which are no whole blocks", image.len());
        let blocks = (image.len() / BLOCK) as u64;
        Self { state: RefCell::new(State { durable: image, unflushed: Vec::new() }), blocks, unit }
    }

    /// Writes no flush has covered.
    pub fn unflushed(&self) -> usize {
        self.state.borrow().unflushed.len()
    }

    /// `judge` asked of every medium a crash now could leave, or of `budget`
    /// of them drawn from `seed` when there are more; the first it refuses.
    /// Each medium is built in place over the durable bytes, so the judge
    /// cannot use this model; one that writes takes an [`Overlay`] over it.
    pub fn check(
        &self,
        budget: u64,
        seed: u64,
        mut judge: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<Search, Violation> {
        let mut state = self.state.borrow_mut();
        let units = self.units(&state);
        let total = units.iter().try_fold(1u64, |n, u| n.checked_mul(u.versions.len() as u64 + 1));
        let search = match total {
            Some(n) if n <= budget => Search::Every(n),
            _ => Search::Sampled { seed, samples: budget },
        };
        let states = match search {
            Search::Every(n) => n,
            Search::Sampled { samples, .. } => samples,
        };
        let State { durable, unflushed } = &mut *state;
        let unit = self.unit;
        let kept: Vec<u8> = units.iter().flat_map(|u| &durable[u.at..u.at + unit]).copied().collect();
        let mut choice = Vec::with_capacity(units.len());
        let mut verdict = Ok(search);
        for index in 0..states {
            choice.clear();
            match search {
                Search::Every(_) => {
                    let mut rest = index;
                    for u in &units {
                        let radix = u.versions.len() as u64 + 1;
                        choice.push((rest % radix) as usize);
                        rest /= radix;
                    }
                }
                Search::Sampled { seed, .. } => {
                    let mut rng = SplitMix(seed ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
                    choice.extend(units.iter().map(|u| (rng.next() % (u.versions.len() as u64 + 1)) as usize));
                }
            }
            // 0 keeps the durable unit, `k` the `k`th version.
            for (i, (u, &c)) in units.iter().zip(&choice).enumerate() {
                let version = match c.checked_sub(1) {
                    Some(k) => {
                        let (w, offset) = u.versions[k];
                        &unflushed[w].data[offset..offset + unit]
                    }
                    None => &kept[i * unit..(i + 1) * unit],
                };
                durable[u.at..u.at + unit].copy_from_slice(version);
            }
            if let Err(why) = judge(durable) {
                verdict = Err(Violation { search, state: index, why, image: durable.clone() });
                break;
            }
        }
        for (u, kept) in units.iter().zip(kept.chunks_exact(unit)) {
            durable[u.at..u.at + unit].copy_from_slice(kept);
        }
        verdict
    }

    fn units(&self, state: &State) -> Vec<Unit> {
        let mut units: Vec<Unit> = Vec::new();
        for (w, write) in state.unflushed.iter().enumerate() {
            for offset in (0..write.data.len()).step_by(self.unit) {
                let at = write.at + offset;
                match units.binary_search_by_key(&at, |u| u.at) {
                    Ok(i) => units[i].versions.push((w, offset)),
                    Err(i) => units.insert(i, Unit { at, versions: alloc::vec![(w, offset)] }),
                }
            }
        }
        units
    }
}

impl BlockDev for Model {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    /// What the device answers now: the durable bytes under every unflushed
    /// write, the latest last.
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        within(lba, out.len(), self.blocks)?;
        let state = self.state.borrow();
        let (start, out) = (lba as usize * BLOCK, out.as_flattened_mut());
        let end = start + out.len();
        out.copy_from_slice(&state.durable[start..end]);
        for w in &state.unflushed {
            let (from, to) = (w.at.max(start), (w.at + w.data.len()).min(end));
            if from < to {
                out[from - start..to - start].copy_from_slice(&w.data[from - w.at..to - w.at]);
            }
        }
        Ok(())
    }

    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        within(lba, data.len(), self.blocks)?;
        let at = lba as usize * BLOCK;
        self.state.borrow_mut().unflushed.push(Unflushed { at, data: data.as_flattened().to_vec() });
        Ok(())
    }

    async fn flush(&self) -> Result<(), Error> {
        let mut state = self.state.borrow_mut();
        let State { durable, unflushed } = &mut *state;
        for w in unflushed.drain(..) {
            durable[w.at..w.at + w.data.len()].copy_from_slice(&w.data);
        }
        Ok(())
    }
}

/// A device over a medium [`Model::check`] judges, which leaves it as it is:
/// what is written here is kept apart and read back over it.
pub struct Overlay<'a> {
    base: &'a [u8],
    written: RefCell<BTreeMap<u64, Block>>,
}

impl<'a> Overlay<'a> {
    pub fn new(base: &'a [u8]) -> Self {
        assert!(base.len().is_multiple_of(BLOCK), "a device over {} bytes, which are no whole blocks", base.len());
        Self { base, written: RefCell::new(BTreeMap::new()) }
    }
}

impl BlockDev for Overlay<'_> {
    fn blocks(&self) -> u64 {
        (self.base.len() / BLOCK) as u64
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        within(lba, out.len(), self.blocks())?;
        let written = self.written.borrow();
        for (block, out) in (lba..).zip(out) {
            match written.get(&block) {
                Some(data) => out.copy_from_slice(data),
                None => out.copy_from_slice(&self.base[block as usize * BLOCK..][..BLOCK]),
            }
        }
        Ok(())
    }

    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        within(lba, data.len(), self.blocks())?;
        self.written.borrow_mut().extend((lba..).zip(data.iter().copied()));
        Ok(())
    }

    async fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
}

/// SplitMix64 (Steele, Lea and Flood, "Fast splittable pseudorandom number
/// generators", OOPSLA 2014): a sample's choices from its seed alone.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on_ready;
    use alloc::collections::BTreeSet;
    use alloc::format;
    use alloc::vec;

    const RECORD: u64 = 1;
    const MARKER: u64 = 2;
    const SEED: u64 = 0x5EED_B10C;

    /// A toy commit: a record, then a marker naming it, with or without a
    /// flush between them.
    async fn commit(dev: &Model, flush_between: bool) {
        dev.write(RECORD, &[[0xAA; BLOCK]]).await.unwrap();
        if flush_between {
            dev.flush().await.unwrap();
        }
        dev.write(MARKER, &[[0x11; BLOCK]]).await.unwrap();
    }

    /// The toy's recovery rule: a marker is never on the medium without the
    /// whole record it names.
    fn committed_is_whole(image: &[u8]) -> Result<(), String> {
        let block = |n: u64| &image[n as usize * BLOCK..][..BLOCK];
        if block(MARKER).iter().any(|&b| b != 0) && block(RECORD).iter().any(|&b| b != 0xAA) {
            return Err("the marker is on the medium and its record is not whole".into());
        }
        Ok(())
    }

    /// The model's own control, red: with no flush between record and
    /// marker, some crash keeps the marker and not the record.
    #[test]
    fn a_marker_written_without_a_flush_after_its_record_is_found_without_it() {
        let dev = Model::new(vec![0; 4 * BLOCK], 512);
        block_on_ready(commit(&dev, false));
        let found = dev.check(1 << 20, SEED, committed_is_whole).expect_err("no crash separated marker and record");
        assert!(matches!(found.search, Search::Every(_)), "{found}");
        assert!(committed_is_whole(&found.image).is_err());
    }

    /// The same control, green: a flush between them leaves no crash in
    /// which the marker is without its record.
    #[test]
    fn a_marker_written_after_its_record_was_flushed_is_never_without_it() {
        let dev = Model::new(vec![0; 4 * BLOCK], 512);
        block_on_ready(commit(&dev, true));
        let search = dev.check(1 << 20, SEED, committed_is_whole).unwrap_or_else(|v| panic!("{v}"));
        assert_eq!(search, Search::Every(1 << 8), "the marker's eight units, each kept or not");
    }

    /// One block written at one LBA on a device with 512-byte units: a crash
    /// keeps any subset of its eight sectors, torn ones included. On a device
    /// that keeps the whole block across a power loss, only none or all.
    #[test]
    fn a_write_at_one_lba_tears_at_the_device_unit_and_only_there() {
        for (unit, states) in [(512, 256), (BLOCK, 2)] {
            let dev = Model::new(vec![0; 2 * BLOCK], unit);
            block_on_ready(dev.write(1, &[[0xFF; BLOCK]])).unwrap();
            let mut seen = BTreeSet::new();
            let search = dev
                .check(1 << 16, SEED, |image| {
                    let kept: u8 = (0..8).fold(0, |k, s| k | u8::from(image[BLOCK + s * 512] == 0xFF) << s);
                    seen.insert(kept);
                    Ok(())
                })
                .unwrap();
            assert_eq!(search, Search::Every(states));
            let want: BTreeSet<u8> = match unit {
                512 => (0..=255).collect(),
                _ => [0, 0xFF].into(),
            };
            assert_eq!(seen, want, "a {unit}-byte unit");
        }
    }

    /// Two writes to one unit, neither flushed: a crash leaves the durable
    /// bytes or either write's, since a cache may have written the older one
    /// and not the newer. A read answers the newer.
    #[test]
    fn an_overwritten_unflushed_unit_may_crash_to_either_version() {
        let dev = Model::new(vec![0; BLOCK], BLOCK);
        let read = block_on_ready(async {
            dev.write(0, &[[1; BLOCK]]).await.unwrap();
            dev.write(0, &[[2; BLOCK]]).await.unwrap();
            let mut out = [[0; BLOCK]];
            dev.read(0, &mut out).await.unwrap();
            out[0][0]
        });
        assert_eq!(read, 2);
        let mut seen = BTreeSet::new();
        dev.check(16, SEED, |image| {
            seen.insert(image[0]);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [0, 1, 2].into());
        block_on_ready(dev.flush()).unwrap();
        let flushed = dev.check(16, SEED, |image| if image[0] == 2 { Ok(()) } else { Err(format!("{}", image[0])) });
        assert_eq!(flushed.unwrap(), Search::Every(1), "a flushed write is the only state");
    }

    /// More states than the budget are sampled from the seed, the same ones
    /// each run, and a refusal names the seed it came from.
    #[test]
    fn past_the_budget_states_are_drawn_from_the_seed_and_a_refusal_names_it() {
        let dev = Model::new(vec![0; 64 * BLOCK], 512);
        block_on_ready(dev.write(0, &[[9; BLOCK]; 64])).unwrap();
        let draw = |seed| {
            let mut images = Vec::new();
            let search = dev
                .check(32, seed, |image| {
                    images.push(image.iter().map(|&b| u32::from(b == 9)).sum::<u32>());
                    Ok(())
                })
                .unwrap();
            (search, images)
        };
        let (search, first) = draw(SEED);
        assert_eq!(search, Search::Sampled { seed: SEED, samples: 32 });
        assert_eq!(draw(SEED).1, first, "a seed draws the same states");
        assert_ne!(draw(SEED + 1).1, first);
        let refused = dev.check(32, SEED, |_| Err("refused".into())).unwrap_err();
        assert_eq!(format!("{refused}"), format!("crash sample 0 of 32 from seed {SEED:#x}: refused"));
    }

    /// A check builds each medium over the durable bytes and leaves them as
    /// they were, whether a judge refused one or not; a judge's writes through
    /// an overlay reach neither.
    #[test]
    fn a_check_and_its_judges_writes_leave_the_durable_bytes_as_they_were() {
        let dev = Model::new(vec![3; 2 * BLOCK], 512);
        block_on_ready(dev.write(1, &[[9; BLOCK]])).unwrap();
        // Which of the write's sectors each state keeps: every subset once, so
        // no state inherits a unit the one before it chose.
        let media = |dev: &Model| {
            let mut seen = BTreeSet::new();
            dev.check(1 << 10, SEED, |image| {
                assert!(seen.insert((0..8).fold(0u8, |k, s| k | u8::from(image[BLOCK + s * 512] == 9) << s)));
                Ok(())
            })
            .unwrap();
            seen
        };
        let every: BTreeSet<u8> = (0..=255).collect();
        assert_eq!(media(&dev), every, "each state is its own subset of the write");
        assert!(dev.check(1 << 10, SEED, |image| if image[BLOCK] == 9 { Err("kept".into()) } else { Ok(()) }).is_err());
        assert_eq!(media(&dev), every, "a refusal left a state behind");
        block_on_ready(dev.write(0, &[[5; BLOCK]])).unwrap();
        dev.check(1, SEED, |image| {
            let overlay = Overlay::new(image);
            block_on_ready(async {
                overlay.write(0, &[[7; BLOCK]]).await.unwrap();
                let mut out = [[0; BLOCK]; 2];
                overlay.read(0, &mut out).await.unwrap();
                assert_eq!((out[0], &out[1][..]), ([7; BLOCK], &image[BLOCK..]));
            });
            (image[0] != 7).then_some(()).ok_or_else(|| "an overlay wrote through".into())
        })
        .unwrap();
    }
}
