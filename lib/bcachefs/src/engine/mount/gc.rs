//! Evacuate sparse closed metadata buckets through ordinary node publication.

use super::*;

impl<D: BlockDev> Mount<D> {
    pub(crate) async fn compact_metadata(&self) -> Result<bool> {
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
