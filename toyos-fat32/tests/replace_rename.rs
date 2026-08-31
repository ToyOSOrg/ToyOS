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
    refused: bool,
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
        if !self.refused && offset == self.destination_entry && cluster == self.source_cluster {
            self.refused = true;
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

#[test]
fn source_move_failure_restores_destination_before_any_free() {
    let source = b"source survives";
    let destination = b"destination survives";
    let mut volume = Volume::new();
    volume.add_file(
        ROOT_CLUSTER,
        "source.txt",
        b"SOURCE  TXT",
        None,
        source.len() as u32,
    );
    volume.add_file(
        ROOT_CLUSTER,
        "destination.txt",
        b"DESTIN~1TXT",
        Some("destination.txt"),
        destination.len() as u32,
    );
    let source_cluster = volume.at("source.txt").first;
    let destination_entry = volume.at("destination.txt").entry as u64;
    volume.poke(cluster_offset(source_cluster), source);
    let destination_cluster = volume.at("destination.txt").first;
    volume.poke(cluster_offset(destination_cluster), destination);
    volume.finish();
    assert!(toyos_fat32_check::check(&volume.bytes).is_empty());

    let dev = RefuseSourceAtDestination {
        bytes: volume.bytes,
        destination_entry,
        source_cluster,
        refused: false,
    };
    let mut fs = Fat32::mount(dev).expect("mount");
    assert!(matches!(
        fs.replace_rename("source.txt", "destination.txt"),
        Err(Error::Io)
    ));
    assert_eq!(read_all(&mut fs, "source.txt"), source);
    assert_eq!(read_all(&mut fs, "destination.txt"), destination);

    let dev = fs.into_device();
    assert!(dev.refused, "the device error did not hit the source move");
    let complaints = toyos_fat32_check::check(&dev.bytes);
    assert!(
        complaints.is_empty(),
        "{}",
        toyos_fat32_check::describe(&complaints)
    );
}
