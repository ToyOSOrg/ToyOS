//! Explicit DATA initialization. The caller holds the selected partition
//! exclusively and prints its contents before invoking `create`.

use bcachefs::engine::{self, Mount, MountOptions, Options, Tree};
use toyos_blockdev::{BLOCK, BlockDev};
use toyos_blockring::wire::Listed;
use toyos_blockserve::content::{self, Content};
use toyos_crypto::Random;
use toyos_gpt::Guid;

pub fn select(listed: &[Listed]) -> Result<Listed, String> {
    let data: Vec<_> = listed
        .iter()
        .filter(|p| p.kind == Guid::TOYOS_DATA.0)
        .copied()
        .collect();
    match data[..] {
        [one] => Ok(one),
        [] => Err("no DATA partition is available".into()),
        _ => Err(format!(
            "{} DATA partitions are available; none was selected",
            data.len()
        )),
    }
}

pub async fn contents(disk: &impl BlockDev) -> Result<String, toyos_blockdev::Error> {
    let blocks = disk.blocks().min((content::READ_BYTES / BLOCK) as u64) as usize;
    let mut front = vec![[0; BLOCK]; blocks];
    disk.read(0, &mut front).await?;
    Ok(match content::judge(front.as_flattened(), disk.blocks()) {
        Content::Volume => "bcachefs".into(),
        Content::Designated(_) => "a ToyOS designation stamp".into(),
        Content::Foreign(what) => what.into(),
        Content::Unrecognised => "unrecognised bytes (not evidence of an empty partition)".into(),
    })
}

pub async fn create(
    disk: &impl BlockDev,
    time: u64,
    mut random: impl Random + 'static,
) -> engine::Result<()> {
    let options = Options {
        label: b"ToyOS DATA",
        time,
        encryption: None,
    };
    engine::mkfs(disk, &options, &Tree::new(), &mut random).await?;
    let mounted = Mount::open(disk, MountOptions { cache_nodes: 512 }, Box::new(random)).await?;
    if !mounted.inode(mounted.root()).await?.is_dir() {
        return Err(engine::Error::Invalid(
            "the formatted root is not a directory",
        ));
    }
    mounted.unmount().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_blockdev::{Bytes, ReadOnlyBytes, block_on_ready};

    struct Seeded(u64);
    impl Random for Seeded {
        fn fill(&mut self, out: &mut [u8]) -> Result<(), toyos_crypto::Error> {
            for byte in out {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                *byte = self.0 as u8;
            }
            Ok(())
        }
    }

    #[test]
    fn selection_refuses_absence_and_ambiguity_including_duplicate_guids() {
        let data = Listed {
            disk: 0,
            unique: [7; 16],
            kind: Guid::TOYOS_DATA.0,
        };
        let foreign = Listed {
            disk: 0,
            unique: [8; 16],
            kind: Guid::MICROSOFT_BASIC.0,
        };
        assert!(select(&[]).is_err());
        assert!(select(&[foreign]).is_err());
        assert_eq!(select(&[foreign, data]), Ok(data));
        assert!(select(&[data, Listed { disk: 1, ..data }]).is_err());
        assert!(
            select(&[
                data,
                Listed {
                    unique: [9; 16],
                    ..data
                }
            ])
            .is_err()
        );
    }

    #[test]
    fn inspection_never_changes_a_recognized_volume_and_explicit_creation_replaces_it() {
        let mut old = vec![0xa5; 64 << 20];
        old[..4].copy_from_slice(b"BCFS");
        let disk = Bytes::new(old);
        assert_eq!(
            block_on_ready(contents(&disk)).unwrap(),
            "the retired ToyOS provisional file system"
        );
        assert_eq!(&disk.get()[..4], b"BCFS");
        assert!(disk.get()[4..].iter().all(|&b| b == 0xa5));
        block_on_ready(create(&disk, 1_700_000_000_000_000_000, Seeded(7))).unwrap();
        assert_eq!(block_on_ready(contents(&disk)).unwrap(), "bcachefs");
        let volume = block_on_ready(engine::Volume::open(&disk)).unwrap();
        assert!(
            block_on_ready(volume.inode(volume.root()))
                .unwrap()
                .is_dir()
        );
    }

    #[test]
    fn a_device_that_refuses_writes_cannot_report_success() {
        let bytes = vec![0; 64 << 20];
        let disk = ReadOnlyBytes::new(&bytes).unwrap();
        assert!(matches!(
            block_on_ready(create(&disk, 1, Seeded(7))),
            Err(engine::Error::Device(toyos_blockdev::Error::ReadOnly))
        ));
        assert!(bytes.iter().all(|&b| b == 0));
    }
}
