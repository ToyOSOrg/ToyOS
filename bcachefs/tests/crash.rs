//! A directory rename, changes after it and the sync that commits them,
//! stopped at every block write: the crash-point model of the commit.
//!
//! **A device keeps what a flush covered and any part of what it did not.**
//! The writes since the last flush may land in any order, so each stop is
//! tried twice: every write of the open epoch up to the stop landed, and the
//! stop's write alone did. What the device then holds must mount, keep every
//! name beside the directory, and name the directory whole under exactly one
//! of its two names.

use std::cell::RefCell;
use std::collections::BTreeMap;

use bcachefs::{BlockBuf, BlockIO, BlockNum, DeviceError, Formatted, FsError, Mounted, ReadOnly, ReadWrite, TransferError, VecBlockIO};

const BLOCKS: u64 = 512;
const BLOCK: usize = 4096;
const STAGED: &str = "staged";
const INSTALLED: &str = "apps/pkg";

/// What the device was asked, in order: a block written, or a flush.
enum Event {
    Write(u64, Box<[u8; BLOCK]>),
    Flush,
}

/// The device as the filesystem sees it, every write in place at once, with
/// the order it was asked in kept beside it; and the write numbered `refuse`,
/// refused.
struct Logged {
    image: RefCell<Vec<u8>>,
    log: RefCell<Vec<Event>>,
    writes: RefCell<usize>,
    refuse: Option<usize>,
}

impl Logged {
    fn new(image: &[u8], refuse: Option<usize>) -> Self {
        Self { image: RefCell::new(image.to_vec()), log: RefCell::default(), writes: RefCell::default(), refuse }
    }
}

struct Refused;
impl TransferError for Refused {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

impl BlockIO for Logged {
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
        let at = block.raw() as usize * BLOCK;
        buf.0.copy_from_slice(&self.image.borrow()[at..at + BLOCK]);
        Ok(())
    }

    fn write_block(&self, block: BlockNum, buf: &BlockBuf) -> Result<(), DeviceError> {
        let n = self.writes.replace_with(|n| *n + 1);
        if Some(n) == self.refuse {
            return Err(DeviceError::classify(&Refused));
        }
        self.log.borrow_mut().push(Event::Write(block.raw(), Box::new(buf.0)));
        let at = block.raw() as usize * BLOCK;
        self.image.borrow_mut()[at..at + BLOCK].copy_from_slice(&buf.0);
        Ok(())
    }

    fn block_count(&self) -> u64 {
        BLOCKS
    }

    fn sync(&self) -> Result<(), DeviceError> {
        self.log.borrow_mut().push(Event::Flush);
        Ok(())
    }
}

fn file(i: usize) -> String {
    if i.is_multiple_of(4) { format!("{STAGED}/sub/f{i}") } else { format!("{STAGED}/f{i}") }
}

/// A committed volume: names beside the directory, and the directory with
/// files enough to span leaves, a subdirectory and an empty one.
fn staged() -> Vec<u8> {
    let mut fs = Formatted::format(VecBlockIO::new(BLOCKS)).expect("format");
    for i in 0..80 {
        fs.create(&format!("beside{i}"), format!("beside {i}").as_bytes(), 1).expect("beside");
    }
    fs.create(&format!("{STAGED}/"), b"", 1).expect("the directory's own entry");
    fs.create(&format!("{STAGED}/empty/"), b"", 1).expect("an empty directory");
    for i in 0..60 {
        fs.create(&file(i), format!("file {i} of the package").as_bytes(), 1).expect("staged");
    }
    fs.into_io().expect("commit").into_vec()
}

/// Every name `keep` accepts, with its bytes, or what would not read.
fn names<M>(fs: &Mounted<impl BlockIO, M>, keep: &dyn Fn(&str) -> bool) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let listed = fs.list(usize::MAX, keep).map_err(|e| format!("the list: {e:?}"))?;
    listed
        .into_iter()
        .map(|(name, _)| fs.read_file(&name).map(|bytes| (name.clone(), bytes)).map_err(|e| format!("{name}: {e:?}")))
        .collect()
}

/// The directory's own entry and every name beneath it, by what follows `dir`.
fn tree<M>(fs: &Mounted<impl BlockIO, M>, dir: &str) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let beneath = format!("{dir}/");
    let all = names(fs, &|n| n.starts_with(&beneath))?;
    Ok(all.into_iter().map(|(name, bytes)| (name[dir.len()..].to_string(), bytes)).collect())
}

fn beside<M>(fs: &Mounted<impl BlockIO, M>) -> Result<BTreeMap<String, Vec<u8>>, String> {
    names(fs, &|n| n.starts_with("beside"))
}

/// The rename: every name beneath the directory, and its own.
fn rename(fs: &mut Mounted<Logged, ReadWrite>) -> Result<(), FsError> {
    let moves: Vec<(String, String)> = tree(fs, STAGED)
        .expect("the directory reads")
        .into_keys()
        .map(|rest| (format!("{STAGED}{rest}"), format!("{INSTALLED}{rest}")))
        .collect();
    let pairs: Vec<(&str, &str)> = moves.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    fs.rename_all(&pairs)
}

/// What a volume must hold: the directory's names and those beside it.
struct Want {
    tree: BTreeMap<String, Vec<u8>>,
    beside: BTreeMap<String, Vec<u8>>,
}

impl Want {
    fn of(image: &[u8]) -> Self {
        let fs = reopened(image.to_vec()).expect("the staged volume mounts");
        Self { tree: tree(&fs, STAGED).unwrap(), beside: beside(&fs).unwrap() }
    }

    /// Where the directory is: `Ok(true)` whole under its new name,
    /// `Ok(false)` whole under its old, and otherwise what was found.
    fn whole_under_one<M>(&self, fs: &Mounted<impl BlockIO, M>) -> Result<bool, String> {
        let kept = beside(fs)?;
        if kept != self.beside {
            return Err(format!("{} of {} names beside the directory", kept.len(), self.beside.len()));
        }
        let (old, new) = (tree(fs, STAGED)?, tree(fs, INSTALLED)?);
        match (old.is_empty(), new.is_empty()) {
            (false, true) if old == self.tree => Ok(false),
            (true, false) if new == self.tree => Ok(true),
            _ => Err(format!("{} names under {STAGED} and {} under {INSTALLED}", old.len(), new.len())),
        }
    }

    fn held(&self, image: Vec<u8>) -> Result<bool, String> {
        reopened(image).and_then(|fs| self.whole_under_one(&fs))
    }
}

fn reopened(image: Vec<u8>) -> Result<Mounted<VecBlockIO, ReadOnly>, String> {
    Mounted::<_, ReadOnly>::open(VecBlockIO::from_vec(image)).map_err(|e| format!("unmountable: {e:?}"))
}

#[test]
fn a_directory_rename_is_whole_under_one_name_wherever_the_device_stops() {
    let image = staged();
    let want = Want::of(&image);
    assert!(want.tree.len() > 60, "the directory holds {} names", want.tree.len());

    let mut fs = Mounted::<_, ReadWrite>::open(Logged::new(&image, None)).expect("mount");
    rename(&mut fs).expect("the rename");
    // Changes after it, which take the blocks the rename gave up if they
    // are handed out before the commit lands.
    for i in 0..40 {
        fs.create(&format!("after{i}"), &[7; BLOCK], 2).expect("a change after it");
    }
    fs.sync().expect("the commit");
    assert_eq!(want.held(fs.io().image.borrow().clone()), Ok(true));
    let log = fs.io().log.replace(Vec::new());

    let writes: Vec<usize> = (0..log.len()).filter(|&i| matches!(log[i], Event::Write(..))).collect();
    assert!(writes.len() > 10, "the run wrote {} blocks", writes.len());
    let mut torn = Vec::new();
    for (k, &stop) in writes.iter().enumerate() {
        let epoch = log[..stop].iter().rposition(|e| matches!(e, Event::Flush)).map_or(0, |f| f + 1);
        for (shape, landed) in [("in order", epoch..=stop), ("alone", stop..=stop)] {
            let mut held = image.clone();
            for event in log[..epoch].iter().chain(&log[landed]) {
                if let Event::Write(block, data) = event {
                    let at = *block as usize * BLOCK;
                    held[at..at + BLOCK].copy_from_slice(&data[..]);
                }
            }
            if let Err(why) = want.held(held) {
                torn.push(format!("stopped at write {k} of {}, the epoch's writes {shape}: {why}", writes.len()));
            }
        }
    }
    assert!(torn.is_empty(), "{} of {} stops tear the volume:\n{}", torn.len(), 2 * writes.len(), torn.join("\n"));
}

/// The same with the one write numbered `n` refused, the filesystem alive
/// after it: the rename's answer is what it serves, the device holds the
/// directory whole under one name past the refused sync, and the next sync
/// makes the answer what the device holds.
#[test]
fn a_directory_rename_is_whole_under_one_name_whichever_write_is_refused() {
    let image = staged();
    let want = Want::of(&image);
    let writes = {
        let mut fs = Mounted::<_, ReadWrite>::open(Logged::new(&image, None)).expect("mount");
        rename(&mut fs).expect("the rename");
        fs.sync().expect("the commit");
        let n = *fs.io().writes.borrow();
        n
    };

    let mut torn = Vec::new();
    for refuse in 0..writes {
        let mut fs = Mounted::<_, ReadWrite>::open(Logged::new(&image, Some(refuse))).expect("mount");
        let renamed = rename(&mut fs).is_ok();
        let served = want.whole_under_one(&fs);
        let first = fs.sync();
        // Changes past a refused sync, which take the blocks of the tree it
        // may have committed if they are handed out before the next lands.
        let changed = (0..40).try_for_each(|i| fs.create(&format!("between{i}"), &[7; 9000], 2));
        let between = want.held(fs.io().image.borrow().clone());
        let second = fs.sync();
        let held = want.held(fs.io().image.borrow().clone());
        if served != Ok(renamed) || changed.is_err() || between.is_err() || second.is_err() || held != Ok(renamed) {
            torn.push(format!(
                "write {refuse} of {writes} refused: renamed {renamed}, served {served:?}, \
                 the sync {first:?}, the changes past it {changed:?}, left {between:?}, the next {second:?} left {held:?}"
            ));
        }
    }
    assert!(torn.is_empty(), "{} of {writes} refusals tear the directory:\n{}", torn.len(), torn.join("\n"));
}
