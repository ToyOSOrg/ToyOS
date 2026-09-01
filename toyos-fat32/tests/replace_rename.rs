//! Replacing rename's device-error window, exercised against the real writer
//! and judged by the independent FAT checker.

#[path = "../../toyos-fat32-check/tests/common/mod.rs"]
mod spec_volume;

use spec_volume::{cluster_offset, Volume, DIR_FST_CLUS_HI, DIR_FST_CLUS_LO, ROOT_CLUSTER};
use toyos_fat32::{BlockAccess, Error, Fat32, IoError};

struct RefuseSourceAtDestination {
    bytes: Vec<u8>,
    destination_entry: u64,
    source_cluster: u32,
    /// Set to refuse the rollback too. Any short entry naming this cluster
    /// after the source's move was refused is the restore's: the staging
    /// rename's own writes are all behind that point.
    rollback_cluster: Option<u32>,
    refused_source: bool,
    refused_rollback: bool,
}

impl RefuseSourceAtDestination {
    fn check(&self, offset: u64, len: usize) -> Result<(usize, usize), IoError> {
        let start = usize::try_from(offset).map_err(|_| IoError::Device)?;
        let end = start.checked_add(len).ok_or(IoError::Device)?;
        if end > self.bytes.len() {
            return Err(IoError::Device);
        }
        Ok((start, end))
    }
}

impl BlockAccess for RefuseSourceAtDestination {
    fn capacity(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let (start, end) = self.check(offset, buf.len())?;
        buf.copy_from_slice(&self.bytes[start..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        let cluster = if buf.len() == 32 {
            let high = u16::from_le_bytes([buf[DIR_FST_CLUS_HI], buf[DIR_FST_CLUS_HI + 1]]) as u32;
            let low = u16::from_le_bytes([buf[DIR_FST_CLUS_LO], buf[DIR_FST_CLUS_LO + 1]]) as u32;
            (high << 16) | low
        } else {
            0
        };
        if !self.refused_source && offset == self.destination_entry && cluster == self.source_cluster {
            self.refused_source = true;
            return Err(IoError::Device);
        }
        if self.refused_source
            && !self.refused_rollback
            && buf.len() == 32
            && self.rollback_cluster == Some(cluster)
        {
            self.refused_rollback = true;
            return Err(IoError::Device);
        }
        let (start, end) = self.check(offset, buf.len())?;
        self.bytes[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}

fn read_all(fs: &mut Fat32<RefuseSourceAtDestination>, path: &str) -> Vec<u8> {
    let mut file = fs.open(path).expect("open file");
    let mut bytes = vec![0; file.len() as usize];
    fs.read(&mut file, 0, &mut bytes).expect("read file");
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

    let dev = RefuseSourceAtDestination {
        bytes: volume.bytes,
        destination_entry,
        source_cluster,
        rollback_cluster: None,
        refused_source: false,
        refused_rollback: false,
    };
    let mut fs = Fat32::mount(dev).expect("mount");
    let failed = fs
        .replace_rename("source.txt", "destination.txt")
        .expect_err("the refused source move must not report success");
    assert_eq!(failed.cause, Error::Io);
    assert_eq!(failed.stranded, None, "the rollback succeeded, so nothing is stranded");
    assert_eq!(read_all(&mut fs, "source.txt"), source);
    assert_eq!(read_all(&mut fs, "destination.txt"), destination);

    let dev = fs.into_device();
    assert!(dev.refused_source, "the device error did not hit the source move");
    let complaints = toyos_fat32_check::check(&dev.bytes);
    assert!(
        complaints.is_empty(),
        "{}",
        toyos_fat32_check::describe(&complaints)
    );
}

/// The rollback fails too, and the caller is told which name holds the
/// destination.
///
/// Nothing on the volume records it: the entry is a valid entry under a valid
/// name, so the checker below is silent in both arms and no fsck pass could
/// ever surface this. The reported name is the only way back to the data.
#[test]
fn a_failed_rollback_names_where_the_destination_went() {
    let source = b"source survives";
    let destination = b"destination survives";
    let (volume, source_cluster, destination_cluster, destination_entry) =
        staged(source, destination);

    let dev = RefuseSourceAtDestination {
        bytes: volume.bytes,
        destination_entry,
        source_cluster,
        rollback_cluster: Some(destination_cluster),
        refused_source: false,
        refused_rollback: false,
    };
    let mut fs = Fat32::mount(dev).expect("mount");
    let failed = fs
        .replace_rename("source.txt", "destination.txt")
        .expect_err("both refusals must reach the caller");
    assert_eq!(failed.cause, Error::Io);
    let stranded = failed.stranded.expect("the failed rollback must name the staging file");
    assert_eq!(stranded, ".toyos-replaced-00000000.tmp");

    // What the report claims, checked against the volume: the destination is
    // not at its own name and its bytes are under the reported one.
    assert_eq!(fs.exists("destination.txt"), Ok(false));
    assert_eq!(read_all(&mut fs, "source.txt"), source);
    assert_eq!(read_all(&mut fs, &stranded), destination);

    let dev = fs.into_device();
    assert!(dev.refused_source && dev.refused_rollback, "both refusals must have fired");
    let complaints = toyos_fat32_check::check(&dev.bytes);
    assert!(
        complaints.is_empty(),
        "{}",
        toyos_fat32_check::describe(&complaints)
    );
}
