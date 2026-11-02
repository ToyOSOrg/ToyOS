//! Evacuate sparse closed metadata buckets through ordinary node publication.

use super::*;
use super::super::value::{csum_type, Extent};

struct Evacuating<'a>(&'a Cell<bool>);
impl Drop for Evacuating<'_> {
    fn drop(&mut self) { self.0.set(false); }
}

struct DataRange<'a, D> {
    mount: &'a Mount<D>,
    sector: u64,
    published: bool,
}

impl<D> Drop for DataRange<'_, D> {
    fn drop(&mut self) {
        let bucket = u64::from(self.mount.sb.member.bucket_sectors);
        let mut st = self.mount.st.borrow_mut();
        if self.published { st.alloc.unpin(self.sector / bucket); } else { st.alloc.cancel_data(self.sector, bucket); }
    }
}

impl<D: BlockDev> Mount<D> {
    /// Called with the file-data slot held, before an ordinary allocation
    /// is accepted. Live bytes are packed; key ranges and counts stay fixed.
    pub(super) async fn compact_user(&self) -> Result<bool> {
        let candidate = {
            let st = self.st.borrow();
            st.sparse_data.iter().copied().find(|&b| st.alloc.movable_data(b))
        };
        let Some(victim) = candidate else { return Ok(false); };
        let bucket = u64::from(self.sb.member.bucket_sectors);
        let first = (victim * bucket) << self.bp_shift();
        let end = ((victim + 1) * bucket) << self.bp_shift();
        let mut positions = Vec::new();
        Source::walk(self, Btree::Backpointers, Bpos::new(0, first, 0), |key, value| {
            if key.pos.inode != 0 || key.pos.offset >= end { return Ok(false); }
            if value.len() != value::BACKPOINTER_BYTES || value[0] != Btree::Extents as u8 || value[1] != 0 || value[2] != data_type::USER {
                return Err(Error::Unsupported("evacuating data other than regular extents"));
            }
            positions.push((Bpos::decode(&value[12..32]), key.pos.offset, value[3], super::super::le32(value, 8)));
            Ok(true)
        }).await?;
        let mut entries = Vec::new();
        for (pos, bp, gen, size) in positions {
            let (key, value) = Source::get(self, Btree::Extents, pos).await?.ok_or(Error::Corrupt("data backpointer has no extent"))?;
            if key.kind != kind::EXTENT || key.size == 0 || pos.snapshot != ROOT_SNAPSHOT { return Err(Error::Unsupported("evacuating a non-live extent")); }
            let extent = data::checked_extent(&value)?;
            if extent.ptr.sector / bucket != victim || (extent.ptr.sector << self.bp_shift()) + u64::from(extent.crc.unwrap().offset) != bp || extent.ptr.gen != gen || key.size != size {
                return Err(Error::Corrupt("data backpointer disagrees with its extent"));
            }
            let bytes = key.size as usize * SECTOR;
            if bytes > 256 << 10 { return Err(Error::Unsupported("evacuating an extent beyond the encoded-size limit")); }
            let mut live = alloc::vec![0u8; bytes];
            // The data slot excludes writes, truncation and final release
            // until these bytes have been copied and their pointers moved.
            Source::extent(self, &key, &value, 0, &mut live).await?;
            entries.push((key, value, live));
        }
        if entries.is_empty() { return Err(Error::Corrupt("nonempty data bucket has no backpointers")); }
        let chunk = (64 << 10).max(self.sb.block_bytes());
        let mut begin = 0;
        while begin < entries.len() {
            let mut end = begin + 1;
            let mut bytes = entries[begin].2.len();
            while end < entries.len() && bytes + entries[end].2.len() <= chunk {
                bytes += entries[end].2.len();
                end += 1;
            }
            let version = self.data_version().await?;
            let physical = bytes.div_ceil(self.sb.block_bytes()) * self.sb.block_bytes();
            let mut blocks = alloc::vec![[0u8; BLOCK]; physical / BLOCK];
            let mut at = 0;
            for (_, _, live) in &entries[begin..end] {
                blocks.as_flattened_mut()[at..at + live.len()].copy_from_slice(live);
                at += live.len();
            }
            let (sector, gen) = self.transact(K_TX, FOOTPRINT, Draw::Freeing, async |txn: &mut Txn| {
                let sector = self.data_sector((physical / SECTOR) as u64, Draw::Freeing, false).await?.0;
                let gen = self.bucket(txn, sector / bucket).await?.reused_gen();
                self.st.borrow_mut().alloc.pin(sector / bucket);
                Ok((sector, gen))
            }).await?;
            let mut range = DataRange { mount: self, sector, published: false };
            let ptr = Ptr { sector, gen, dev: 0, cached: false, unwritten: false };
            let encoded = self.encode_data(&mut blocks, version, ptr, true)?;
            let crc = Extent::decode(&encoded)?.crc.unwrap();
            let mut replacements = Vec::new();
            let mut offset = 0;
            for (old, value, live) in &entries[begin..end] {
                let mut crc = crc;
                crc.offset = offset;
                if self.key.is_some() && data::checked_extent(value)?.crc.unwrap().csum_type == csum_type::CHACHA_POLY_80 {
                    crc.csum_type = csum_type::CHACHA_POLY_80;
                    crc.csum_len = 10;
                }
                let value_new = Extent::with_crc(&crc, ptr);
                if value_new.len() > value.len() { return Err(Error::Unsupported("evacuation would enlarge an extent record")); }
                replacements.push((Key { version, ..*old }, value_new));
                offset += (live.len() / SECTOR) as u32;
            }
            self.reissue("data evacuation write failed", async || {
                self.dev.write_data(sector * SECTOR as u64 / BLOCK as u64, &blocks).await?;
                Ok(())
            }).await?;
            self.st.borrow_mut().clocks[1] += (physical / SECTOR) as u64;
            for ((old, value, _), new) in entries[begin..end].iter().zip(replacements) {
                self.transact(K_TX, FOOTPRINT, Draw::Freeing, async |txn: &mut Txn| {
                    let mut inode = self.inode_in(txn, old.pos.inode).await?;
                    self.replace_data_keys(txn, &mut inode, alloc::vec![(*old, value.clone())], alloc::vec![new]).await
                }).await?;
                range.published = true;
            }
            begin = end;
        }
        self.commit().await?;
        Ok(true)
    }

    pub(crate) async fn compact_metadata(&self) -> Result<bool> {
        if self.metadata_gc.replace(true) { return Ok(false); }
        let _evacuating = Evacuating(&self.metadata_gc);
        let candidates: Vec<_> = {
            let st = self.st.borrow();
            st.sparse_metadata.iter().copied().filter(|&b| !st.alloc.metadata_open(b)).collect()
        };
        let bucket = u64::from(self.sb.member.bucket_sectors);
        for b in candidates {
            let first = b * bucket;
            let end = (first + bucket) << self.bp_shift();
            let mut pointers = Vec::new();
            Source::walk(self, Btree::Backpointers, Bpos::new(0, first << self.bp_shift(), 0), |key, value| {
                if key.pos.inode != 0 || key.pos.offset >= end { return Ok(false); }
                if value.len() != value::BACKPOINTER_BYTES || value[2] != data_type::BTREE || value[1] == 0 {
                    return Err(Error::Corrupt("metadata bucket holds a non-metadata backpointer"));
                }
                let btree = Btree::from_id(u32::from(value[0])).ok_or(Error::Corrupt("backpointer btree"))?;
                pointers.push((btree, value[1] - 1, Bpos::decode(&value[12..32]), key.pos.offset >> self.bp_shift()));
                Ok(true)
            }).await?;
            for (btree, level, pos, sector) in pointers {
                if self.relocate_node(btree, level, pos, sector).await? { return Ok(true); }
            }
        }
        Ok(false)
    }
}
