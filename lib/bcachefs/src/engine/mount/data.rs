//! Copy-on-write file data. A pointer is journaled only after its bytes
//! have completed; journal preflush orders those bytes before the pointer.

use super::super::crypt::Nonce;
use super::super::value::{csum_type, Extent};
use super::*;

const VERSIONS: u64 = 1024;

/// The largest extent this writer makes, upstream's default: what a read of
/// one block past the page cache verifies at most.
const EXTENT_MAX: u64 = 64 << 10;

/// What authenticates an extent's bytes as they lie on the device.
#[derive(Clone, Copy)]
enum Seal {
    Crc(u32),
    Tag([u8; 16]),
}

/// One extent of a reserved write: `n` bytes at `at`, in the whole blocks
/// `first..end` of the file, sealed in `blocks` of the write's buffer for
/// `sector`.
struct Piece {
    at: u64,
    n: usize,
    first: u64,
    end: u64,
    blocks: core::ops::Range<usize>,
    sector: u64,
    version: Version,
    seal: Seal,
    /// It replaces no old extent.
    fresh: bool,
}

/// A write onto reserved sectors, planned: its pieces in file order, and the
/// buffer their sealed blocks lie in.
#[derive(Default)]
struct Plan {
    pieces: Vec<Piece>,
    buf: Vec<[u8; BLOCK]>,
}

/// The most extents of one bucket one transaction points at (fresh ones of
/// a reserved write) or removes (whole ones a truncation passes). The
/// group shares one change of its bucket: its alloc key, its LRU entry,
/// which moves once from the fill before to the fill after, and its
/// accounting. Each extent past the first adds only its extent key and
/// its backpointer, in at most one node more of each of those two trees.
pub(crate) const POINTED: usize = 8;

/// Contiguous sectors for consecutive blocks of a file, each written once.
struct Run {
    first: u64,
    sector: u64,
    blocks: u32,
    written: u128,
}

pub(super) struct Reserved {
    ino: u64,
    block_sectors: u64,
    runs: Vec<Run>,
    /// A pool ([`Mount::reserve_blocks`]): its blocks are placed wherever
    /// each write lands, and it outlives being spent, to grow again.
    pool: bool,
}

impl Reserved {
    fn unwritten(&self) -> u64 {
        self.runs.iter().map(|r| u64::from(r.blocks - r.written.count_ones())).sum()
    }
}

#[derive(Default)]
pub(super) struct Reservations {
    pub(super) active: BTreeMap<u64, Reserved>,
    canceled: Vec<u64>,
    next: u64,
}

impl Reservations {
    /// Metadata credit the others hold: a transaction's worth for each
    /// block not yet written, which a part's own transaction may then spend.
    pub(super) fn pending(&self, draw: Draw) -> u64 {
        let own = match draw {
            Draw::Reserved(id) => Some(id),
            _ => None,
        };
        self.active.iter().filter(|(&id, _)| Some(id) != own).map(|(_, r)| r.unwritten()).sum::<u64>() * K_TX * 8
    }
}

/// Capacity for the whole blocks of a range: runs of contiguous sectors of at
/// most [`EXTENT_MAX`]. Dropping it returns the blocks never written on the
/// next operation.
pub struct Reservation {
    owner: Rc<RefCell<Reservations>>,
    id: u64,
    first: u64,
    bytes: u64,
}

impl Reservation {
    pub fn range(&self) -> (u64, u64) {
        (self.first, self.first + self.bytes)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut pool = self.owner.borrow_mut();
        if let Some(reserved) = pool.active.remove(&self.id) {
            for run in &reserved.runs {
                for block in (0..run.blocks).filter(|b| run.written & 1 << b == 0) {
                    pool.canceled.push(run.sector + u64::from(block) * reserved.block_sectors);
                }
            }
        }
    }
}

pub(super) struct DataSlot<'a, D>(&'a Mount<D>);

impl<D> Drop for DataSlot<'_, D> {
    fn drop(&mut self) {
        self.0.data_held.set(false);
        self.0.data_queue.wake();
    }
}

fn start(k: &Key) -> Result<u64> {
    k.pos.offset.checked_sub(u64::from(k.size)).and_then(|s| s.checked_mul(SECTOR as u64)).ok_or(Error::Corrupt("extent start past its end"))
}

fn regular(inode: &InodeValue) -> Result<()> {
    regular_mode(inode.mode())
}

fn regular_mode(bits: u16) -> Result<()> {
    match bits & mode::S_IFMT {
        mode::S_IFREG | mode::S_IFLNK => Ok(()),
        mode::S_IFDIR => Err(Error::IsDirectory),
        _ => Err(Error::Invalid("file data on a special inode")),
    }
}

impl<D: BlockDev> Mount<D> {
    pub(super) fn cancel_dropped(&self) {
        let canceled = core::mem::take(&mut self.reservations.borrow_mut().canceled);
        let mut st = self.st.borrow_mut();
        for sector in canceled {
            st.alloc.cancel_data(sector, u64::from(self.sb.member.bucket_sectors));
        }
    }

    /// Reserves the whole blocks of `[offset, offset + len)`, a run of
    /// contiguous sectors at a time: a later write into it cannot be refused
    /// for space.
    pub async fn reserve(&self, ino: u64, offset: u64, len: usize) -> Result<Reservation> {
        self.reserve_as(ino, offset, len, false).await
    }

    /// [`Self::reserve`], of a pool where `pool`.
    async fn reserve_as(&self, ino: u64, offset: u64, len: usize, is_pool: bool) -> Result<Reservation> {
        let block = self.sb.block_bytes() as u64;
        let first = offset / block * block;
        let end = offset.checked_add(len as u64).filter(|&end| len > 0 && i64::try_from(end).is_ok()).ok_or(Error::Invalid("reservation of nothing or past 2^63-1"))?;
        let id = {
            let mut pool = self.reservations.borrow_mut();
            let id = pool.next;
            pool.next = pool.next.checked_add(1).expect("reservation identifiers exhausted");
            pool.active.insert(id, Reserved { ino, block_sectors: block / SECTOR as u64, runs: Vec::new(), pool: is_pool });
            id
        };
        let mut reservation = Reservation { owner: Rc::clone(&self.reservations), id, first, bytes: 0 };
        self.extend(&mut reservation, (end - first) as usize).await?;
        Ok(reservation)
    }

    /// Reserves the whole blocks of the `len` bytes past `reservation`'s end
    /// into it: a write that continues the last one, whose blocks then join
    /// its last run where the sectors follow on in the same bucket, so a
    /// write-back of both is one extent.
    pub async fn extend(&self, reservation: &mut Reservation, len: usize) -> Result<()> {
        if !Rc::ptr_eq(&reservation.owner, &self.reservations) {
            return Err(Error::Invalid("reservation belongs to another mount"));
        }
        let block = self.sb.block_bytes() as u64;
        let at_end = reservation.range().1;
        let end = at_end.checked_add(len as u64).filter(|&end| len > 0 && i64::try_from(end).is_ok()).ok_or(Error::Invalid("reservation of nothing or past 2^63-1"))?.div_ceil(block) * block;
        let _data = self.data_slot().await;
        // Whole or refused: what a refused extension took goes back.
        let taken = self.take_reserved(reservation, end).await;
        if taken.is_err() {
            self.cut(reservation, at_end);
        }
        taken
    }

    /// Takes runs into `reservation` up to `end`. Each take is judged for
    /// the metadata credit it adds, a transaction's for each of its blocks,
    /// as each may be written back alone.
    async fn take_reserved(&self, reservation: &mut Reservation, end: u64) -> Result<()> {
        let block = self.sb.block_bytes() as u64;
        let bucket = u64::from(self.sb.member.bucket_sectors);
        let longest = EXTENT_MAX.min(bucket * SECTOR as u64) / block * block;
        let ino = self.reservations.borrow().active.get(&reservation.id).ok_or(Error::Invalid("reservation already written"))?.ino;
        regular_mode(self.inode(ino).await?.mode)?;
        let (from, mut at) = reservation.range();
        while at < end {
            let wanted = (end - at).min(longest) / SECTOR as u64;
            let single = wanted * SECTOR as u64 == block;
            let credit = K_TX * 8 * (wanted * SECTOR as u64 / block);
            // Sectors of the open bucket are taken in memory where room for
            // their writes is plain; a new bucket, or a judgement that may
            // wait or refuse, is a transaction's.
            let taken = if self.room_for(credit, FOOTPRINT) { self.st.borrow_mut().alloc.next_data(bucket, wanted, single, !single).map(|(sector, _, got)| (sector, got)) } else { None };
            let (sector, got) = match taken {
                Some(taken) => taken,
                // The grant judges and holds the take's whole credit, so a
                // bucket it opens is judged with it too.
                None => loop {
                    let out = self
                        .transact_holding(K_TX, credit, FOOTPRINT, Draw::Ordinary, async |_: &mut Txn| {
                            if single { self.data_sector(wanted, Draw::Ordinary, true).await.map(|(sector, _)| (sector, wanted)) } else { self.data_run(wanted, Draw::Ordinary).await }
                        })
                        .await;
                    match out {
                        Err(Error::NoSpace(_)) if self.compact_user().await? => continue,
                        other => break other?,
                    }
                },
            };
            let blocks = (got * SECTOR as u64 / block) as u32;
            {
                let mut st = self.st.borrow_mut();
                for _ in 0..blocks { st.alloc.pin(sector / bucket); }
            }
            let mut pool = self.reservations.borrow_mut();
            let runs = &mut pool.active.get_mut(&reservation.id).expect("a reservation being made").runs;
            match runs.last_mut() {
                Some(last) if last.first + u64::from(last.blocks) * block == at
                    && last.sector + u64::from(last.blocks) * block / SECTOR as u64 == sector
                    && last.sector / bucket == sector / bucket
                    && u64::from(last.blocks + blocks) * block <= longest => last.blocks += blocks,
                _ => runs.push(Run { first: at, sector, blocks, written: 0 }),
            }
            at += got * SECTOR as u64;
            reservation.bytes = at - from;
            drop(pool);
            #[cfg(test)]
            assert!(self.room_for(0, FOOTPRINT), "a take promised more than the room keeps");
        }
        Ok(())
    }

    /// Gives back the blocks of `reservation` from the one holding `to` on,
    /// none of them written: what a write that extended it and was refused
    /// had taken.
    pub fn cut(&self, reservation: &mut Reservation, to: u64) {
        let block = self.sb.block_bytes() as u64;
        let to = to.max(reservation.first).div_ceil(block) * block;
        let mut pool = self.reservations.borrow_mut();
        let Some(reserved) = pool.active.get_mut(&reservation.id) else { return };
        let mut canceled = Vec::new();
        reserved.runs.retain_mut(|run| {
            let kept = (to.saturating_sub(run.first) / block).min(u64::from(run.blocks)) as u32;
            for b in kept..run.blocks {
                assert!(run.written & 1 << b == 0, "a reserved block cut after its write");
                canceled.push(run.sector + u64::from(b) * reserved.block_sectors);
            }
            run.blocks = kept;
            kept > 0
        });
        pool.canceled.extend(canceled);
        reservation.bytes = reservation.bytes.min(to - reservation.first);
    }

    /// Writes into blocks of the reservation not yet written, as one extent
    /// for each run the bytes cross; a block written in part keeps the rest
    /// of its bytes, and counts as written. Bytes written; a
    /// failure after some returns their count, and the rest stay reserved
    /// for a retry until dropped.
    ///
    /// The sectors are known before it starts, so the bytes go to the device
    /// first, each stretch of contiguous sectors as one write, and the
    /// extents' pointers follow, up to [`POINTED`] fresh ones of a bucket a
    /// transaction.
    pub async fn write_reserved(&self, reservation: &mut Reservation, offset: u64, data: &[u8], now: i64) -> Result<usize> {
        if !Rc::ptr_eq(&reservation.owner, &self.reservations) {
            return Err(Error::Invalid("reservation belongs to another mount"));
        }
        let block = self.sb.block_bytes() as u64;
        if offset < reservation.first || data.is_empty() || offset.checked_add(data.len() as u64).is_none_or(|end| end > reservation.range().1) {
            return Err(Error::Invalid("write outside its reserved blocks"));
        }
        let _slot = self.data_slot().await;
        self.check_writable()?;
        self.observe(now);
        let ino = self.reservations.borrow().active.get(&reservation.id).ok_or(Error::Invalid("reservation already written"))?.ino;
        regular_mode(self.inode(ino).await?.mode)?;
        let mut plan = Plan::default();
        let mut planned = 0;
        while planned < data.len() {
            let at = offset + planned as u64;
            // The extent starts at the block holding `at`, and so do its sectors.
            let (sector, end) = {
                let pool = self.reservations.borrow();
                let reserved = pool.active.get(&reservation.id).expect("a reservation being written");
                let run = reserved.runs.iter().find(|r| (r.first..r.first + u64::from(r.blocks) * block).contains(&at)).expect("a reservation covers its range");
                let end = (offset + data.len() as u64).min(run.first + u64::from(run.blocks) * block);
                let (from, to) = (((at - run.first) / block) as u32, ((end - run.first).div_ceil(block)) as u32);
                if (from..to).any(|b| run.written & 1 << b != 0) {
                    return Err(Error::Invalid("a reserved block written twice"));
                }
                (run.sector + (at / block * block - run.first) / SECTOR as u64, end)
            };
            planned += self.plan_piece(&mut plan, ino, at, &data[planned..(end - offset) as usize], sector).await?;
        }
        self.send_and_point(reservation.id, ino, plan, now).await
    }

    /// One piece of a write onto reserved `sector`: the whole blocks `input`
    /// at `at` spans, or as many as one transaction replaces, cut and sealed
    /// into `plan`. Bytes it takes.
    async fn plan_piece(&self, plan: &mut Plan, ino: u64, at: u64, input: &[u8], sector: u64) -> Result<usize> {
        let version = self.data_version().await?;
        let (first, end, old) = self.span(ino, at, input.len()).await?;
        let n = (input.len() as u64).min(end - at) as usize;
        let blocks = plan.buf.len()..plan.buf.len() + (end - first) as usize / BLOCK;
        plan.buf.resize(blocks.end, [0; BLOCK]);
        self.fill(ino, first, end, at, &input[..n], plan.buf[blocks.clone()].as_flattened_mut()).await?;
        let seal = self.seal(&mut plan.buf[blocks.clone()], version)?;
        plan.pieces.push(Piece { at, n, first, end, blocks, sector, version, seal, fresh: old.is_empty() });
        Ok(n)
    }

    /// A planned write onto reservation `id`'s sectors: its bytes to the
    /// device first, each stretch of contiguous sectors as one write, then
    /// its extents' pointers, up to [`POINTED`] fresh ones of a bucket a
    /// transaction, each block counted written as its pointer lands. Bytes
    /// written; a failure after some returns their count. A reservation
    /// with nothing left unwritten goes, and a pool keeps only its runs not
    /// written whole.
    async fn send_and_point(&self, id: u64, ino: u64, plan: Plan, now: i64) -> Result<usize> {
        let block = self.sb.block_bytes() as u64;
        let bucket = u64::from(self.sb.member.bucket_sectors);
        let Plan { pieces, buf } = plan;
        for stretch in pieces.chunk_by(|a, b| b.sector == a.sector + (a.end - a.first) / SECTOR as u64) {
            let (head, last) = (&stretch[0], &stretch[stretch.len() - 1]);
            let bytes = &buf[head.blocks.start..last.blocks.end];
            self.reissue("file data write failed", async || {
                self.dev.write_data(head.sector * SECTOR as u64 / BLOCK as u64, bytes).await?;
                Ok(())
            })
            .await?;
        }
        drop(buf);
        let mut done = 0;
        let groups = pieces.chunk_by(|a, b| a.fresh && b.fresh && a.sector / bucket == b.sector / bucket).flat_map(|together| together.chunks(POINTED));
        for group in groups {
            let pointed = self
                .transact(K_TX, FOOTPRINT, Draw::Reserved(id), async |txn: &mut Txn| {
                    let mut inode = self.inode_in(txn, ino).await?;
                    regular(&inode)?;
                    match group {
                        // An earlier piece may have left a slice of an old
                        // extent this one replaces: its old extents are read
                        // again here.
                        [piece] if !piece.fresh => {
                            let (first, end, old) = self.span(ino, piece.at, piece.n).await?;
                            assert_eq!((first, end), (piece.first, piece.end), "a file's extents changed under its reserved write");
                            self.point_extent(txn, ino, &mut inode, first..end, old, piece.version, piece.sector, piece.seal).await?;
                        }
                        // A fresh piece's blocks held no extent when it was
                        // planned, and no earlier piece leaves a slice there:
                        // an old extent reaching them would have made it not
                        // fresh. The group's one bucket changes once for all.
                        _ => {
                            let gen = self.bucket(txn, group[0].sector / bucket).await?.reused_gen();
                            let new = group.iter().map(|p| self.new_extent(ino, p.first..p.end, p.version, Ptr { sector: p.sector, dev: 0, gen, cached: false, unwritten: false }, p.seal)).collect();
                            self.replace_data_keys(txn, &mut inode, Vec::new(), new).await?;
                        }
                    }
                    inode.size = group.iter().fold(inode.size, |size, p| size.max(p.at + p.n as u64));
                    self.touch(&mut inode, now);
                    self.set_inode(txn, ino, &inode).await
                })
                .await;
            match pointed {
                Ok(()) => {}
                Err(_) if done > 0 => return Ok(done),
                Err(e) => return Err(e),
            }
            let mut pool = self.reservations.borrow_mut();
            let mut st = self.st.borrow_mut();
            let reserved = pool.active.get_mut(&id).expect("a reservation being written");
            let block_sectors = reserved.block_sectors;
            for piece in group {
                let run = reserved
                    .runs
                    .iter_mut()
                    .find(|r| (r.sector..r.sector + u64::from(r.blocks) * block_sectors).contains(&piece.sector))
                    .expect("the run just written");
                let first = ((piece.sector - run.sector) / block_sectors) as u32;
                for b in first..first + ((piece.end - piece.first) / block) as u32 {
                    run.written |= 1 << b;
                    st.alloc.unpin(run.sector / bucket);
                }
                done += piece.n;
            }
            if reserved.pool {
                // A pool lives as long as its file is written, and grows
                // again once spent: only its runs written whole go.
                reserved.runs.retain(|r| r.written.count_ones() < r.blocks);
            } else if reserved.unwritten() == 0 {
                pool.active.remove(&id);
            }
        }
        Ok(done)
    }

    /// The bytes of one block: what each block of a pool holds.
    pub fn block_bytes(&self) -> u64 {
        self.sb.block_bytes() as u64
    }

    /// Capacity for `blocks` whole blocks of `ino`, none of them bound to a
    /// place in the file: each is placed where [`Self::write_placed`] writes
    /// it. A pool for pages dirtied anywhere in a file, which a write into
    /// one range ([`Self::reserve`]) cannot serve.
    pub async fn reserve_blocks(&self, ino: u64, blocks: u64) -> Result<Reservation> {
        let len = blocks.checked_mul(self.sb.block_bytes() as u64).and_then(|len| usize::try_from(len).ok());
        self.reserve_as(ino, 0, len.ok_or(Error::Invalid("a pool past 2^63-1 bytes"))?, true).await
    }

    /// `blocks` more blocks into a pool [`Self::reserve_blocks`] made: whole
    /// or refused, as [`Self::extend`] is.
    pub async fn extend_blocks(&self, pool: &mut Reservation, blocks: u64) -> Result<()> {
        let len = blocks.checked_mul(self.sb.block_bytes() as u64).and_then(|len| usize::try_from(len).ok());
        self.extend(pool, len.ok_or(Error::Invalid("a pool past 2^63-1 bytes"))?).await
    }

    /// Gives back `blocks` of a pool's unwritten blocks, or all it has where
    /// it has fewer: how many went.
    pub fn cut_blocks(&self, pool: &mut Reservation, blocks: u64) -> u64 {
        let cut = blocks.min(self.unwritten_blocks(pool));
        // Placed writes take the lowest blocks first, so the highest are unwritten.
        self.cut(pool, pool.range().1 - cut * self.sb.block_bytes() as u64);
        cut
    }

    /// The blocks of `reservation` not yet written.
    pub fn unwritten_blocks(&self, reservation: &Reservation) -> u64 {
        self.reservations.borrow().active.get(&reservation.id).map_or(0, Reserved::unwritten)
    }

    /// Writes `data` at `offset` into the lowest blocks of a pool
    /// ([`Self::reserve_blocks`]) not yet written, wherever they lie, as
    /// [`Self::write_reserved`] writes into its reserved blocks: one extent
    /// for each run of them the bytes take, sent before any is pointed. A
    /// pool with too few blocks left writes what fits: bytes written, refused
    /// where none fits.
    pub async fn write_placed(&self, pool: &mut Reservation, offset: u64, data: &[u8], now: i64) -> Result<usize> {
        if !Rc::ptr_eq(&pool.owner, &self.reservations) {
            return Err(Error::Invalid("reservation belongs to another mount"));
        }
        let block = self.sb.block_bytes() as u64;
        if data.is_empty() || offset.checked_add(data.len() as u64).is_none_or(|end| i64::try_from(end).is_err()) {
            return Err(Error::Invalid("a placed write of nothing or past 2^63-1"));
        }
        let _slot = self.data_slot().await;
        self.check_writable()?;
        self.observe(now);
        // Each run's sectors and the blocks already written or taken by a
        // piece planned here: the cursor the pieces take the lowest from.
        let (ino, block_sectors, mut runs) = {
            let reservations = self.reservations.borrow();
            let reserved = reservations.active.get(&pool.id).ok_or(Error::Invalid("a pool with no block left"))?;
            (reserved.ino, reserved.block_sectors, reserved.runs.iter().map(|r| (r.sector, r.blocks, r.written)).collect::<Vec<_>>())
        };
        regular_mode(self.inode(ino).await?.mode)?;
        let end = offset + data.len() as u64;
        let mut plan = Plan::default();
        let mut planned = 0;
        while planned < data.len() {
            let at = offset + planned as u64;
            let wanted = (end.div_ceil(block) - at / block) as u32;
            let lowest = runs.iter().enumerate().find_map(|(i, &(_, blocks, taken))| (0..blocks).find(|b| taken & 1 << b == 0).map(|b| (i, b)));
            let Some((i, first)) = lowest else { break };
            let (sector, blocks, taken) = runs[i];
            let free = (first..blocks).take_while(|b| taken & 1 << b == 0).count() as u32;
            let stop = end.min((at / block + u64::from(free.min(wanted))) * block);
            let n = self.plan_piece(&mut plan, ino, at, &data[planned..(stop - offset) as usize], sector + u64::from(first) * block_sectors).await?;
            let piece = plan.pieces.last().expect("a piece just planned");
            for b in first..first + ((piece.end - piece.first) / block) as u32 {
                runs[i].2 |= 1 << b;
            }
            planned += n;
        }
        if plan.pieces.is_empty() {
            return Err(Error::Invalid("a pool with no block left"));
        }
        self.send_and_point(pool.id, ino, plan, now).await
    }

    pub(super) async fn create_symlink(&self, dir: u64, name: &[u8], target: &[u8], owner: Owner, now: i64) -> Result<Inode> {
        if target.is_empty() || target.len() >= 4096 || target.contains(&0) {
            return Err(Error::Invalid("symlink target empty, holding NUL or past 4095 bytes"));
        }
        let ino = self.create_unnamed(dir, owner, now).await?.ino;
        let out = async {
            self.transact(K_TX / 2, 2, Draw::Ordinary, async |txn: &mut Txn| {
                let mut inode = self.inode_in(txn, ino).await?;
                inode.flags = (inode.flags & !(u64::from(mode::S_IFMT) << 36)) | u64::from(mode::S_IFLNK) << 36;
                self.set_inode(txn, ino, &inode).await
            })
            .await?;
            let mut bytes = target.to_vec();
            bytes.push(0);
            if self.write(ino, 0, &bytes, now).await? != bytes.len() {
                return Err(Error::NoSpace("symlink target"));
            }
            self.link(ino, dir, name, Link::New, now).await?;
            self.inode(ino).await
        }
        .await;
        if out.is_err() {
            self.release(ino).await?;
        }
        out
    }

    pub(super) async fn data_slot(&self) -> DataSlot<'_, D> {
        self.data_queue.wait(|| (!self.data_held.get()).then(|| self.data_held.set(true))).await;
        DataSlot(self)
    }

    pub async fn read(&self, ino: u64, offset: u64, out: &mut [u8]) -> Result<usize> {
        Source::read(self, ino, offset, out).await
    }

    pub async fn readlink(&self, ino: u64) -> Result<Vec<u8>> {
        Source::readlink(self, ino).await
    }

    /// Writes as many bytes as fit, an extent of at most [`EXTENT_MAX`] at a
    /// time. A later extent's failure returns the completed byte count;
    /// `commit` is the durability barrier.
    pub async fn write(&self, ino: u64, offset: u64, data: &[u8], now: i64) -> Result<usize> {
        offset.checked_add(data.len() as u64).filter(|n| i64::try_from(*n).is_ok()).ok_or(Error::Invalid("file size past 2^63-1"))?;
        let _slot = self.data_slot().await;
        self.observe(now);
        let block = self.sb.block_bytes() as u64;
        let longest = EXTENT_MAX.min(u64::from(self.sb.member.bucket_sectors) * SECTOR as u64) / block * block;
        let mut done = 0;
        while done < data.len() {
            let at = offset + done as u64;
            let room = (at / block * block + longest - at) as usize;
            let part = &data[done..done + room.min(data.len() - done)];
            let out = match self.data_version().await {
                Ok(version) => self.write_run(ino, at, part, now, version).await,
                Err(e) => Err(e),
            };
            let out = match out {
                Err(error @ Error::NoSpace(_)) => match self.compact_user().await {
                    Ok(true) => continue,
                    Ok(false) => Err(error),
                    Err(error) => Err(error),
                },
                other => other,
            };
            match out {
                Ok(n) => done += n,
                Err(_) if done > 0 => return Ok(done),
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }

    pub async fn set_mtime(&self, ino: u64, now: i64) -> Result<()> {
        self.observe(now);
        self.transact(K_TX / 2, 2, Draw::Ordinary, async |txn: &mut Txn| {
            let mut inode = self.inode_in(txn, ino).await?;
            inode.set(inode_field::MTIME, self.stored_time(now));
            self.set_inode(txn, ino, &inode).await
        })
        .await
    }

    /// Total bytes and capacity left after pending data and metadata claims.
    pub fn space(&self) -> (u64, u64) {
        self.cancel_dropped();
        let bucket = u64::from(self.sb.member.bucket_sectors) * SECTOR as u64;
        let node = u64::from(self.sb.node_sectors()) * SECTOR as u64;
        let st = self.st.borrow();
        let pending = self.reservations.borrow().pending(Draw::Ordinary);
        let stable = st.alloc.stable_slots(self.metadata_nodes(&st, st.granted_bytes + pending));
        let transient = Allocator::transient(st.cache.dirty + st.cache.unpointed + st.granted + FOOTPRINT, st.reclaim_room, Draw::Ordinary);
        let logical = st.alloc.capacity(st.metadata_buckets).saturating_sub(stable + transient) * node;
        (self.sb.member.nbuckets * bucket, st.alloc.free_bytes(bucket).min(logical))
    }

    pub(super) fn metadata_nodes(&self, st: &State, pending_bytes: u64) -> u64 {
        let buckets = u128::from(self.sb.member.nbuckets);
        let slots = buckets * u128::from(self.sb.member.bucket_sectors) / u128::from(self.sb.node_sectors());
        // Each bucket has at most one alloc, freespace, discard and LRU
        // record; each node slot one metadata backpointer. Generation
        // records cover 256 buckets. Remaining fixed accounting records
        // are bounded by the format's classes and btrees.
        let structural = buckets * (104 + 3 * 40) + buckets.div_ceil(256) * 296 + slots * 72 + (Btree::ALL.len() * 9 * 64) as u128;
        let bytes = u128::from(st.metadata_bytes) + structural + u128::from(pending_bytes);
        let minimum = self.minimum_fill();
        // A tree may retain its at-most-four-node whole-range root spine.
        // Other interior nodes have at least two children after rebalance.
        let stable = st.baseline_nodes.len() as u128 + 4 * Btree::ALL.len() as u128 + 2 * bytes.div_ceil(minimum as u128);
        stable.min(u128::from(u64::MAX)) as u64
    }

    #[cfg(test)]
    pub(crate) async fn check_metadata_budget(&self) {
        let metadata = allocator::metadata_budget(self).await.unwrap();
        let st = self.st.borrow();
        assert_eq!((st.metadata_bytes, st.metadata_buckets, st.btree_nodes), (metadata.bytes, metadata.buckets, metadata.nodes));
        assert!(st.btree_nodes <= st.alloc.occupied_metadata(st.metadata_buckets));
        assert_eq!(st.sparse_metadata, metadata.sparse);
        assert_eq!(st.sparse_data, metadata.sparse_data);
    }

    /// The transactions applied since the last ask, and the largest of them:
    /// its journal u64s and the nodes it changed.
    #[cfg(test)]
    pub(crate) fn test_applied(&self) -> (u64, [u64; 2]) {
        let mut st = self.st.borrow_mut();
        (core::mem::take(&mut st.applied), core::mem::take(&mut st.largest))
    }

    #[cfg(test)]
    pub(crate) fn test_reserve_buckets(&self, extra_metadata: u64) -> u64 {
        let st = self.st.borrow();
        let stable = st.alloc.stable_slots(self.metadata_nodes(&st, extra_metadata + K_TX * 8));
        let transient = Allocator::transient(FOOTPRINT, st.reclaim_room, Draw::Ordinary);
        let per_bucket = u64::from(self.sb.member.bucket_sectors) / u64::from(self.sb.node_sectors());
        (stable + transient + (reclaim::COMMON_REWRITE * FOOTPRINT) as u64).div_ceil(per_bucket)
    }

    #[cfg(test)]
    pub(crate) async fn test_compact_user(&self) -> Result<bool> {
        let _data = self.data_slot().await;
        self.compact_user().await
    }

    /// The high-water mark is durable before any version beneath it is
    /// used, including a ciphertext write whose metadata never commits.
    pub(super) async fn data_version(&self) -> Result<Version> {
        if self.key.is_none() {
            return Ok(Version::default());
        }
        if self.st.borrow().next_version == self.st.borrow().key_version {
            self.transact(0, 0, Draw::Freeing, async |_txn: &mut Txn| {
                let mut st = self.st.borrow_mut();
                st.key_version = st.key_version.checked_add(VERSIONS).ok_or(Error::NoSpace("encrypted extent versions"))?;
                Ok(())
            })
            .await?;
            self.commit().await?;
        }
        let mut st = self.st.borrow_mut();
        st.next_version += 1;
        Ok(Version { hi: 0, lo: st.next_version })
    }

    async fn extent_after(&self, ino: u64, byte: u64) -> Result<Option<(Key, Vec<u8>)>> {
        let mut found = None;
        Source::walk(self, Btree::Extents, Bpos::new(ino, byte / SECTOR as u64 + 1, 0), |k, v| {
            if k.pos.inode != ino {
                return Ok(false);
            }
            if k.pos.snapshot == ROOT_SNAPSHOT {
                found = Some((*k, v.to_vec()));
            }
            Ok(found.is_none())
        })
        .await?;
        Ok(found)
    }

    /// The whole blocks `first..end` an extent of `len` bytes at `at` spans,
    /// and the old extents it replaces. A transaction removes at most two old
    /// extents, as one block's write always could, so a write over more ends
    /// before the third.
    async fn span(&self, ino: u64, at: u64, len: usize) -> Result<(u64, u64, Vec<(Key, Vec<u8>)>)> {
        let block = self.sb.block_bytes() as u64;
        let first = at / block * block;
        let mut end = (at + len as u64).div_ceil(block) * block;
        let mut old = self.extents_in(ino, first, end, 3).await?;
        if old.len() == 3 {
            end = start(&old[2].0)? / block * block;
            if end <= first {
                return Err(Error::Unsupported("more than two data extents in one filesystem block"));
            }
            old.truncate(2);
            old.retain(|(k, _)| start(k).is_ok_and(|s| s < end));
        }
        Ok((first, end, old))
    }

    /// The blocks `first..end` as they read once `input` lies at `at`: the
    /// first and last read first where it covers them in part.
    async fn fill(&self, ino: u64, first: u64, end: u64, at: u64, input: &[u8], bytes: &mut [u8]) -> Result<()> {
        let block = self.sb.block_bytes() as u64;
        if at > first {
            Source::read(self, ino, first, &mut bytes[..block as usize]).await?;
        }
        if (at + input.len() as u64) < end {
            let last = (end - block - first) as usize;
            Source::read(self, ino, end - block, &mut bytes[last..]).await?;
        }
        bytes[(at - first) as usize..(at - first) as usize + input.len()].copy_from_slice(input);
        Ok(())
    }

    /// One extent of `input` at `at` onto new sectors: the whole blocks it
    /// spans, or as many as the open bucket has left. Bytes written.
    async fn write_run(&self, ino: u64, at: u64, input: &[u8], now: i64, version: Version) -> Result<usize> {
        self.transact(K_TX, FOOTPRINT, Draw::Ordinary, async |txn: &mut Txn| {
            let mut inode = self.inode_in(txn, ino).await?;
            regular(&inode)?;
            let block = self.sb.block_bytes() as u64;
            let (first, mut end, mut old) = self.span(ino, at, input.len()).await?;
            let (sector, got) = if end - first == block {
                (self.data_sector(block / SECTOR as u64, Draw::Ordinary, true).await?.0, block / SECTOR as u64)
            } else {
                self.data_run((end - first) / SECTOR as u64, Draw::Ordinary).await?
            };
            end = first + got * SECTOR as u64;
            old.retain(|(k, _)| start(k).is_ok_and(|s| s < end));
            self.st.borrow_mut().alloc.pin(sector / u64::from(self.sb.member.bucket_sectors));
            txn.allocated_data.push(sector);
            let n = (input.len() as u64).min(end - at) as usize;
            let mut data = alloc::vec![[0u8; BLOCK]; (end - first) as usize / BLOCK];
            self.fill(ino, first, end, at, &input[..n], data.as_flattened_mut()).await?;
            let seal = self.seal(&mut data, version)?;
            self.reissue("file data write failed", async || {
                self.dev.write_data(sector * SECTOR as u64 / BLOCK as u64, &data).await?;
                Ok(())
            })
            .await?;
            self.point_extent(txn, ino, &mut inode, first..end, old, version, sector, seal).await?;
            inode.size = inode.size.max(at + n as u64);
            self.touch(&mut inode, now);
            self.set_inode(txn, ino, &inode).await?;
            Ok(n)
        })
        .await
    }

    pub(super) async fn data_sector(&self, sectors: u64, draw: Draw, fixed: bool) -> Result<(u64, u8)> {
        self.data_space(sectors, draw, fixed, false).await.map(|(sector, gen, _)| (sector, gen))
    }

    /// Up to `sectors` contiguous data sectors, as many as the open bucket
    /// has left before another is taken: where an extent ends.
    async fn data_run(&self, sectors: u64, draw: Draw) -> Result<(u64, u64)> {
        self.data_space(sectors, draw, false, true).await.map(|(sector, _, got)| (sector, got))
    }

    async fn data_space(&self, sectors: u64, draw: Draw, fixed: bool, partial: bool) -> Result<(u64, u8, u64)> {
        let bucket = u64::from(self.sb.member.bucket_sectors);
        if sectors > bucket {
            return Err(Error::Unsupported("data extent larger than a bucket"));
        }
        if let Some(at) = self.st.borrow_mut().alloc.next_data(bucket, sectors, fixed, partial) {
            return Ok(at);
        }
        {
            let st = self.st.borrow();
            let held = st.cache.dirty + st.cache.unpointed + st.granted;
            let stable = self.metadata_nodes(&st, st.granted_bytes + self.reservations.borrow().pending(Draw::Ordinary));
            if !st.alloc.data_fits(held, st.reclaim_room, draw) || !st.alloc.keeps(st.metadata_buckets, stable, held, st.reclaim_room, draw, true) {
                return Err(Error::NoSpace("no data bucket beyond metadata and freeing reserves"));
            }
        }
        self.take_bucket(true).await?;
        Ok(self.st.borrow_mut().alloc.next_data(bucket, sectors, fixed, partial).expect("a fresh bucket holds the extent"))
    }

    /// Up to `most` live extents of `ino` overlapping `[first, end)`, in
    /// order, each of a kind this writer can replace on this volume.
    async fn extents_in(&self, ino: u64, first: u64, end: u64, most: usize) -> Result<Vec<(Key, Vec<u8>)>> {
        let mut old = Vec::new();
        Source::walk(self, Btree::Extents, Bpos::new(ino, first / SECTOR as u64 + 1, 0), |k, v| {
            if k.pos.inode != ino || start(k)? >= end {
                return Ok(false);
            }
            if k.pos.snapshot == ROOT_SNAPSHOT {
                old.push((*k, v.to_vec()));
            }
            Ok(old.len() < most)
        })
        .await?;
        for (k, v) in &old {
            match k.kind {
                kind::EXTENT => {
                    let extent = checked_extent(v)?;
                    let authenticated = matches!(extent.crc.unwrap().csum_type, csum_type::CHACHA_POLY_80 | csum_type::CHACHA_POLY_128);
                    if self.key.is_some() && !authenticated { return Err(Error::Corrupt("unauthenticated extent on an encrypted volume")); }
                    if self.key.is_none() && authenticated { return Err(Error::Locked); }
                }
                kind::INLINE_DATA | kind::WHITEOUT | kind::EXTENT_WHITEOUT => {}
                _ => return Err(Error::Unsupported("modifying an extent type this writer cannot account")),
            }
        }
        Ok(old)
    }

    /// The blocks `range` of `ino` as one new extent at `sector`, where their
    /// bytes lie sealed as `seal`, in place of `old`, whose parts outside the
    /// range stay.
    #[allow(clippy::too_many_arguments)]
    async fn point_extent(&self, txn: &mut Txn, ino: u64, inode: &mut InodeValue, range: core::ops::Range<u64>, old: Vec<(Key, Vec<u8>)>, version: Version, sector: u64, seal: Seal) -> Result<()> {
        let mut retained = Vec::new();
        for (k, v) in &old {
            let from = start(k)?;
            let to = k.pos.offset * SECTOR as u64;
            if from < range.start {
                retained.push(slice(k, v, from, range.start)?);
            }
            if to > range.end {
                retained.push(slice(k, v, range.end, to)?);
            }
        }
        let gen = self.bucket(txn, sector / u64::from(self.sb.member.bucket_sectors)).await?.reused_gen();
        retained.push(self.new_extent(ino, range, version, Ptr { sector, dev: 0, gen, cached: false, unwritten: false }, seal));
        self.replace_data_keys(txn, inode, old, retained).await
    }

    /// The extent of `ino`'s blocks `range` whose bytes lie at `ptr`, sealed
    /// as `seal`.
    fn new_extent(&self, ino: u64, range: core::ops::Range<u64>, version: Version, ptr: Ptr, seal: Seal) -> (Key, Vec<u8>) {
        let physical = ((range.end - range.start) / SECTOR as u64) as u32;
        let value = sealed(physical, seal, ptr, self.sb.flag(1, 9, 1) != 0);
        let pos = Bpos::new(ino, range.end / SECTOR as u64, ROOT_SNAPSHOT);
        self.st.borrow_mut().clocks[1] += u64::from(physical);
        (Key { size: physical, version, ..Key::new(kind::EXTENT, pos) }, value)
    }

    /// `data` as it goes to the device, encrypted in place on an encrypted
    /// volume, and what authenticates it there.
    fn seal(&self, data: &mut [[u8; BLOCK]], version: Version) -> Result<Seal> {
        Ok(if let Some(master) = &self.key {
            let nonce = Nonce::extent(version.hi, version.lo, 0);
            master.xor(nonce, data.as_flattened_mut())?;
            Seal::Tag(master.mac(nonce, data.as_flattened())?)
        } else {
            if data.len() * BLOCK / SECTOR > 512 {
                return Err(Error::Unsupported("unencrypted extent past crc64 maximum"));
            }
            Seal::Crc(crate::crc32c::crc32c_zero(data.as_flattened()))
        })
    }

    pub(super) fn encode_data(&self, data: &mut [[u8; BLOCK]], version: Version, ptr: Ptr, wide: bool) -> Result<Vec<u8>> {
        let physical = (data.len() * BLOCK / SECTOR) as u32;
        Ok(sealed(physical, self.seal(data, version)?, ptr, wide))
    }

    pub(super) async fn replace_data_keys(&self, txn: &mut Txn, inode: &mut InodeValue, old: Vec<(Key, Vec<u8>)>, new: Vec<(Key, Vec<u8>)>) -> Result<()> {
        let mut buckets = BTreeMap::new();
        for (k, v) in old {
            self.data_counted(txn, &mut buckets, &k, &v, false).await?;
            self.set(txn, Btree::Extents, k.pos, None).await?;
            inode.sectors = inode.sectors.checked_sub(allocated_sectors(&k)).ok_or(Error::Corrupt("inode sectors below its extents"))?;
        }
        for (k, v) in new {
            self.data_counted(txn, &mut buckets, &k, &v, true).await?;
            inode.sectors += allocated_sectors(&k);
            self.set(txn, Btree::Extents, k.pos, Some((k, v))).await?;
        }
        self.data_buckets(txn, buckets).await?;
        Ok(())
    }

    async fn data_counted(&self, txn: &mut Txn, buckets: &mut BTreeMap<u64, (Bucket, i64)>, k: &Key, v: &[u8], added: bool) -> Result<()> {
        if k.kind != kind::EXTENT {
            return Ok(());
        }
        let extent = checked_extent(v)?;
        let crc = extent.crc.unwrap();
        let bucket_sectors = u64::from(self.sb.member.bucket_sectors);
        let b = extent.ptr.sector / bucket_sectors;
        if let alloc::collections::btree_map::Entry::Vacant(slot) = buckets.entry(b) {
            let before = self.bucket(txn, b).await?;
            slot.insert((before, 0));
        }
        let delta = &mut buckets.get_mut(&b).unwrap().1;
        if added {
            *delta += i64::from(k.size);
            txn.filled.push(b);
        } else {
            *delta -= i64::from(k.size);
        }
        let pos = Bpos::new(0, (extent.ptr.sector << self.bp_shift()) + u64::from(crc.offset), 0);
        let bp = added.then(|| (Key::new(kind::BACKPOINTER, pos), value::backpointer(Btree::Extents as u8, 0, data_type::USER, extent.ptr.gen, k.size, k.pos).to_vec()));
        self.put(txn, Btree::Backpointers, 0, pos, bp).await?;
        let sign = |v: u64| if added { v } else { v.wrapping_neg() };
        self.account(txn, &[class::REPLICAS, data_type::USER, 1, 1, 0], &[sign(u64::from(k.size))]).await?;
        let mut record = alloc::vec![class::INUM];
        record.extend_from_slice(&k.pos.inode.to_le_bytes());
        self.account(txn, &record, &[sign(1), sign(u64::from(k.size)), sign(u64::from(k.size))]).await
    }

    async fn data_buckets(&self, txn: &mut Txn, buckets: BTreeMap<u64, (Bucket, i64)>) -> Result<()> {
        for (b, (before, delta)) in buckets {
            if delta == 0 {
                continue;
            }
            let mut after = before;
            if delta > 0 {
                after.fill_as(data_type::USER, delta as u32, self.st.borrow().clocks[1], self.sb.member.bucket_sectors as u32)?;
            } else {
                after.drain_as(data_type::USER, (-delta) as u32)?;
            }
            self.bucket_changed(txn, b, before, after).await?;
        }
        Ok(())
    }

    pub async fn truncate(&self, ino: u64, size: u64, now: i64) -> Result<()> {
        if size > i64::MAX as u64 {
            return Err(Error::Invalid("file size past 2^63-1"));
        }
        let _slot = self.data_slot().await;
        self.observe(now);
        let op = self
            .transact(K_TX / 2, 3, Draw::Freeing, async |txn: &mut Txn| {
                let mut inode = self.inode_in(txn, ino).await?;
                regular(&inode)?;
                self.touch(&mut inode, now);
                if size >= inode.size {
                    inode.size = size;
                    self.set_inode(txn, ino, &inode).await?;
                    return Ok(None);
                }
                self.set_inode(txn, ino, &inode).await?;
                let mut pos = Bpos::MIN;
                while self.get_in(txn, Btree::LoggedOps, pos).await?.is_some() {
                    pos.offset = pos.offset.checked_add(1).ok_or(Error::NoSpace("logged operation slots"))?;
                }
                let mut value = alloc::vec![0u8; 24];
                value[..4].copy_from_slice(&mkfs::ROOT_SUBVOLUME.to_le_bytes());
                value[8..16].copy_from_slice(&ino.to_le_bytes());
                value[16..24].copy_from_slice(&size.to_le_bytes());
                self.put(txn, Btree::LoggedOps, 0, pos, Some((Key::new(kind::LOGGED_OP_TRUNCATE, pos), value))).await?;
                Ok(Some(pos))
            })
            .await?;
        if let Some(pos) = op {
            let out = self.finish_truncate(pos, ino, size).await;
            if out.is_err() {
                self.stop("an interrupted truncate needs recovery");
            }
            out?;
        }
        Ok(())
    }

    async fn finish_truncate(&self, pos: Bpos, ino: u64, size: u64) -> Result<()> {
        self.transact(K_TX / 2, 2, Draw::Freeing, async |txn: &mut Txn| {
            let mut inode = self.inode_in(txn, ino).await?;
            regular(&inode)?;
            inode.size = size;
            self.set_inode(txn, ino, &inode).await
        })
        .await?;
        self.trim_extents(ino, size).await?;
        self.transact(K_TX / 2, 1, Draw::Freeing, async |txn: &mut Txn| self.put(txn, Btree::LoggedOps, 0, pos, None).await).await
    }

    pub(super) async fn trim_extents(&self, ino: u64, size: u64) -> Result<()> {
        let end = size.div_ceil(SECTOR as u64) * SECTOR as u64;
        if !size.is_multiple_of(SECTOR as u64) && self.extent_after(ino, size).await?.is_some_and(|(k, _)| start(&k).is_ok_and(|first| first < end)) {
            self.transact(K_TX, FOOTPRINT, Draw::Freeing, async |txn: &mut Txn| {
                let mut inode = self.inode_in(txn, ino).await?;
                let first = size / SECTOR as u64 * SECTOR as u64;
                let (key, value) = self.extent_after(ino, first).await?.ok_or(Error::Corrupt("truncate tail extent disappeared"))?;
                let mut bytes = alloc::vec![0u8; SECTOR];
                Source::read(self, ino, first, &mut bytes).await?;
                bytes[(size - first) as usize..].fill(0);
                let mut retained = Vec::new();
                let from = start(&key)?;
                if from < first {
                    retained.push(slice(&key, &value, from, first)?);
                }
                let pos = Bpos::new(ino, end / SECTOR as u64, ROOT_SNAPSHOT);
                retained.push((Key { size: 1, ..Key::new(kind::INLINE_DATA, pos) }, bytes));
                self.replace_data_keys(txn, &mut inode, alloc::vec![(key, value)], retained).await?;
                self.set_inode(txn, ino, &inode).await
            })
            .await?;
        }
        loop {
            let gone = self.extents_past(ino, end).await?;
            let Some((key, value)) = gone.first() else {
                return Ok(());
            };
            let first = start(key)?;
            self.transact(K_TX, FOOTPRINT, Draw::Freeing, async |txn: &mut Txn| {
                let mut inode = self.inode_in(txn, ino).await?;
                let mut buckets = BTreeMap::new();
                for (key, value) in &gone {
                    self.data_counted(txn, &mut buckets, key, value, false).await?;
                    self.set(txn, Btree::Extents, key.pos, None).await?;
                    inode.sectors = inode.sectors.checked_sub(allocated_sectors(key)).ok_or(Error::Corrupt("inode sectors below its extents"))?;
                }
                if first < end {
                    let (k, v) = slice(key, value, first, end)?;
                    self.data_counted(txn, &mut buckets, &k, &v, true).await?;
                    inode.sectors += allocated_sectors(&k);
                    self.set(txn, Btree::Extents, k.pos, Some((k, v))).await?;
                }
                self.data_buckets(txn, buckets).await?;
                self.set_inode(txn, ino, &inode).await
            })
            .await?;
        }
    }

    /// The first live extent of `ino` reaching past `end`, and with it, where
    /// it lies whole past `end`, those that follow it in its bucket, up to
    /// [`POINTED`]: what one transaction removes.
    async fn extents_past(&self, ino: u64, end: u64) -> Result<Vec<(Key, Vec<u8>)>> {
        let bucket = u64::from(self.sb.member.bucket_sectors);
        let within = |v: &[u8]| checked_extent(v).map(|extent| extent.ptr.sector / bucket);
        let mut found: Vec<(Key, Vec<u8>)> = Vec::new();
        Source::walk(self, Btree::Extents, Bpos::new(ino, end / SECTOR as u64 + 1, 0), |k, v| {
            if k.pos.inode != ino {
                return Ok(false);
            }
            if k.pos.snapshot != ROOT_SNAPSHOT {
                return Ok(true);
            }
            let joins = match found.first() {
                None => true,
                Some((head, value)) => head.kind == kind::EXTENT && k.kind == kind::EXTENT && start(head)? >= end && within(value)? == within(v)?,
            };
            if joins {
                found.push((*k, v.to_vec()));
            }
            Ok(joins && found.len() < POINTED)
        })
        .await?;
        Ok(found)
    }

    pub(super) async fn resume_truncates(&self) -> Result<()> {
        loop {
            let mut op = None;
            Source::walk(self, Btree::LoggedOps, Bpos::MIN, |k, v| {
                if k.pos.inode == 0 {
                    op = Some((*k, v.to_vec()));
                }
                Ok(false)
            })
            .await?;
            let Some((k, v)) = op else {
                return Ok(());
            };
            let (ino, size) = truncate_value(&k, &v)?;
            self.finish_truncate(k.pos, ino, size).await?;
        }
    }
}

/// An extent's value: `physical` sectors at `ptr`, sealed as `seal`.
fn sealed(physical: u32, seal: Seal, ptr: Ptr, wide: bool) -> Vec<u8> {
    match seal {
        Seal::Tag(tag) => Extent::authenticated(physical, tag, wide, ptr),
        Seal::Crc(crc) => Extent::encode(physical, csum_type::CRC32C, crc, ptr),
    }
}

pub(super) fn checked_extent(v: &[u8]) -> Result<Extent> {
    let extent = Extent::decode(v)?;
    let crc = extent.crc.ok_or(Error::Unsupported("modifying data without a checksum entry"))?;
    if extent.ptr.dev != 0 || extent.ptr.cached || extent.ptr.unwritten || crc.compression != 0 || crc.compressed != crc.uncompressed {
        return Err(Error::Unsupported("modifying cached, unwritten, compressed or other-device data"));
    }
    match crc.csum_type {
        csum_type::CRC32C_NONZERO | csum_type::CRC32C => {}
        csum_type::CHACHA_POLY_80 | csum_type::CHACHA_POLY_128 if crc.csum_len == csum_type::bytes(crc.csum_type) => {}
        _ => return Err(Error::Unsupported("modifying data with a checksum this reader cannot verify")),
    }
    Ok(extent)
}

fn allocated_sectors(k: &Key) -> u64 {
    if matches!(k.kind, kind::EXTENT | kind::INLINE_DATA) {
        u64::from(k.size)
    } else {
        0
    }
}

fn slice(k: &Key, v: &[u8], first: u64, end: u64) -> Result<(Key, Vec<u8>)> {
    let old_first = start(k)?;
    let skip = ((first - old_first) / SECTOR as u64) as u32;
    let size = ((end - first) / SECTOR as u64) as u32;
    let value = match k.kind {
        kind::EXTENT => {
            let extent = checked_extent(v)?;
            let mut crc = extent.crc.unwrap();
            crc.offset += skip;
            Extent::with_crc(&crc, extent.ptr)
        }
        kind::INLINE_DATA => {
            let at = skip as usize * SECTOR;
            let n = (size as usize * SECTOR).min(v.len().saturating_sub(at));
            v.get(at..at + n).unwrap_or(&[]).to_vec()
        }
        kind::WHITEOUT | kind::EXTENT_WHITEOUT => Vec::new(),
        _ => return Err(Error::Unsupported("slicing an extent type this writer cannot account")),
    };
    Ok((Key { pos: Bpos { offset: end / SECTOR as u64, ..k.pos }, size, ..*k }, value))
}

pub(super) fn truncate_value(k: &Key, v: &[u8]) -> Result<(u64, u64)> {
    if k.kind != kind::LOGGED_OP_TRUNCATE {
        return Err(Error::Unsupported("logged operation other than truncate"));
    }
    if v.len() != 24 || super::super::le32(v, 0) != mkfs::ROOT_SUBVOLUME || super::super::le32(v, 4) != 0 {
        return Err(Error::Corrupt("logged truncate value"));
    }
    let size = super::super::le64(v, 16);
    if size > i64::MAX as u64 {
        return Err(Error::Corrupt("logged truncate size past 2^63-1"));
    }
    Ok((super::super::le64(v, 8), size))
}
