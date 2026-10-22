//! Replacing rename's device-error window, exercised against the real writer
//! and judged by the independent FAT checker.

#[path = "../check/tests/common/mod.rs"]
mod spec_volume;

use spec_volume::{cluster_offset, Volume, DIR_FST_CLUS_HI, DIR_FST_CLUS_LO, ROOT_CLUSTER};
use std::cell::{Cell, RefCell};

use toyos_blockdev::{block_on_ready as now, within, Block, BlockDev, Error as DevError, BLOCK};
use toyos_fat32::{Error, Fat32};

/// The volume, padded to whole blocks, refusing the first write that puts the
/// source's cluster in the destination's entry and, when asked, the first
/// after it that makes a live entry name `rollback_cluster`. A write is told
/// by the entries it changes, since it reaches the device as whole blocks.
struct RefuseSourceAtDestination {
    bytes: RefCell<Vec<u8>>,
    /// The volume's own length.
    len: usize,
    destination_entry: u64,
    source_cluster: u32,
    /// Set to refuse the rollback too. Any live short entry naming this
    /// cluster after the source's move was refused is the restore's: the
    /// staging rename's own writes are all behind that point, and the refused
    /// move's repair writes the slots it took back as erased ones.
    rollback_cluster: Option<u32>,
    refused_source: Cell<bool>,
    refused_rollback: Cell<bool>,
}

impl RefuseSourceAtDestination {
    fn new(mut bytes: Vec<u8>, destination_entry: u64, source_cluster: u32, rollback_cluster: Option<u32>) -> Self {
        let len = bytes.len();
        bytes.resize(len.next_multiple_of(BLOCK), 0);
        Self {
            bytes: RefCell::new(bytes),
            len,
            destination_entry,
            source_cluster,
            rollback_cluster,
            refused_source: Cell::new(false),
            refused_rollback: Cell::new(false),
        }
    }

    fn volume(&self) -> Vec<u8> {
        self.bytes.borrow()[..self.len].to_vec()
    }

    /// Whether the write of `after` at byte `at` is one this device refuses.
    fn refuses(&self, at: usize, after: &[u8]) -> bool {
        let bytes = self.bytes.borrow();
        let changed = (0..after.len()).step_by(32).filter(|&i| bytes[at + i..at + i + 32] != after[i..i + 32]);
        for i in changed {
            let entry = &after[i..i + 32];
            let high = u16::from_le_bytes([entry[DIR_FST_CLUS_HI], entry[DIR_FST_CLUS_HI + 1]]) as u32;
            let low = u16::from_le_bytes([entry[DIR_FST_CLUS_LO], entry[DIR_FST_CLUS_LO + 1]]) as u32;
            let cluster = (high << 16) | low;
            if !self.refused_source.get() && (at + i) as u64 == self.destination_entry && cluster == self.source_cluster {
                self.refused_source.set(true);
                return true;
            }
            if self.refused_source.get()
                && !self.refused_rollback.get()
                && entry[0] != 0xE5
                && self.rollback_cluster == Some(cluster)
            {
                self.refused_rollback.set(true);
                return true;
            }
        }
        false
    }
}

impl BlockDev for RefuseSourceAtDestination {
    fn blocks(&self) -> u64 {
        (self.bytes.borrow().len() / BLOCK) as u64
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), DevError> {
        within(lba, out.len(), self.blocks())?;
        let (at, out) = (lba as usize * BLOCK, out.as_flattened_mut());
        out.copy_from_slice(&self.bytes.borrow()[at..at + out.len()]);
        Ok(())
    }

    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), DevError> {
        within(lba, data.len(), self.blocks())?;
        let (at, data) = (lba as usize * BLOCK, data.as_flattened());
        if self.refuses(at, data) {
            return Err(DevError::Device);
        }
        self.bytes.borrow_mut()[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    async fn flush(&self) -> Result<(), DevError> {
        Ok(())
    }
}

fn read_all(fs: &mut Fat32<RefuseSourceAtDestination>, path: &str) -> Vec<u8> {
    let mut file = now(fs.open(path)).expect("open file");
    let mut bytes = vec![0; file.len() as usize];
    now(fs.read(&mut file, 0, &mut bytes)).expect("read file");
    bytes
}

/// `source.txt` and `destination.txt`, each with its own cluster of content,
/// on a volume the checker passes before the driver touches it.
fn staged(source: &[u8], destination: &[u8]) -> (Volume, u32, u32, u64) {
    let mut volume = Volume::new();
    volume.add_file(ROOT_CLUSTER, "source.txt", b"SOURCE  TXT", None, source.len() as u32);
    volume.add_file(
        ROOT_CLUSTER,
        "destination.txt",
        b"DESTIN~1TXT",
        Some("destination.txt"),
        destination.len() as u32,
    );
    let source_cluster = volume.at("source.txt").first;
    let destination_entry = volume.at("destination.txt").entry as u64;
    let destination_cluster = volume.at("destination.txt").first;
    volume.poke(cluster_offset(source_cluster), source);
    volume.poke(cluster_offset(destination_cluster), destination);
    volume.finish();
    assert!(toyos_fat32_check::check(&volume.bytes).is_empty());
    (volume, source_cluster, destination_cluster, destination_entry)
}

#[test]
fn source_move_failure_restores_destination_before_any_free() {
    let source = b"source survives";
    let destination = b"destination survives";
    let (volume, source_cluster, _, destination_entry) = staged(source, destination);

    let dev = RefuseSourceAtDestination::new(volume.bytes, destination_entry, source_cluster, None);
    let mut fs = now(Fat32::mount(dev)).expect("mount");
    let failed = now(fs.replace_rename("source.txt", "destination.txt"))
        .expect_err("the refused source move must not report success");
    assert_eq!(failed.cause, Error::Io);
    assert_eq!(failed.stranded, None, "the rollback succeeded, so nothing is stranded");
    assert_eq!(read_all(&mut fs, "source.txt"), source);
    assert_eq!(read_all(&mut fs, "destination.txt"), destination);

    let dev = fs.into_device();
    assert!(dev.refused_source.get(), "the device error did not hit the source move");
    let complaints = toyos_fat32_check::check(&dev.volume());
    assert!(
        complaints.is_empty(),
        "{}",
        toyos_fat32_check::describe(&complaints)
    );
}

/// The rollback fails too, and the caller is told which name holds the
/// destination — which is the only way back to it, the checker below being
/// silent in both arms because a staging entry breaks no rule of the format.
#[test]
fn a_failed_rollback_names_where_the_destination_went() {
    let source = b"source survives";
    let destination = b"destination survives";
    let (volume, source_cluster, destination_cluster, destination_entry) =
        staged(source, destination);

    let dev = RefuseSourceAtDestination::new(volume.bytes, destination_entry, source_cluster, Some(destination_cluster));
    let mut fs = now(Fat32::mount(dev)).expect("mount");
    let failed = now(fs.replace_rename("source.txt", "destination.txt"))
        .expect_err("both refusals must reach the caller");
    assert_eq!(failed.cause, Error::Io);
    let stranded = failed.stranded.expect("the failed rollback must name the staging file");
    assert_eq!(stranded, ".toyos-replaced-00000000.tmp");

    // What the report claims, checked against the volume.
    assert_eq!(now(fs.exists("destination.txt")), Ok(false));
    assert_eq!(read_all(&mut fs, "source.txt"), source);
    assert_eq!(read_all(&mut fs, &stranded), destination);

    let dev = fs.into_device();
    assert!(dev.refused_source.get() && dev.refused_rollback.get(), "both refusals must have fired");
    let complaints = toyos_fat32_check::check(&dev.volume());
    assert!(
        complaints.is_empty(),
        "{}",
        toyos_fat32_check::describe(&complaints)
    );
}
