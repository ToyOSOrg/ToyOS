//! A file's bytes in memory: one paged object of the kernel's per file
//! ([`toyos::paged`]), which its holders read without asking this server and
//! which is the only copy of the file's data the server keeps. This moves
//! pages between the object and the volume: a page a reader asks for is read
//! off the volume and supplied, a write lands dirty in the object, and a sync
//! writes the dirty pages back before the volume's own sync. Memory volumes
//! use anonymous objects instead: no writeback or eviction can discard them.
//!
//! **An object per file, by its volume identity**, which the notices name it
//! by. It holds the volume's node for its life, so a dirty file stays open
//! on the volume whoever closed it, and outlives its last close for its pages'
//! sake until room is wanted, or until a volume on a disk keeps its count of
//! files: then a file nobody holds and nothing dirties goes first, oldest
//! first. Anonymous objects stay while named or held.
//!
//! **One write at a time.** Every write, truncation and read of the server's
//! runs under the object's hold (`toyos::paged::Pager::hold`), so to holders
//! and to each other each is one whole, however many calls it takes; a
//! holder's write that meets one comes here and waits its turn.
//!
//! A write reserves on the volume first, so the volume's size runs past the
//! object's only for the bytes the write then puts in before it returns;
//! nothing reads the volume for them
//! meanwhile, and a write refused part way is cut back to what it put.
//!
//! **Holders write too**, without a request, against a credit of pages this
//! server granted on a volume that holds room for them ([`Volume::grant`]):
//! [`BULK`] at a time while a holder minted to write lives, taken back when
//! the last holder goes. Their writes move the object's size and time past
//! the volume's, so while it has an object a file's size and time are the
//! object's ([`Files::meta`]), which the volume takes at each write-back. A
//! holder's dirtying is told once ([`Notice::Dirtied`]); a write-back, a sync
//! or the last holder's going takes the pages it dirtied from the object.
//!
//! **A server with no pager** (ROOT's, whose files are immutable) reads and
//! writes the volume directly, and each open holds its own node.

use std::collections::BTreeMap;

use toyos::fs::{Error, Record};
use toyos::paged::{Notice, PagedStatus, Refused, PAGE, PAGED_RANGE_PAGES, PAGED_RECORD};
use toyos::shm::Bytes;

use crate::volume::{Buf, Kind, Meta, Node, Reserved, Volume, Window, Writes};

/// The most pages one supply or write-back carries.
const RANGE: u64 = PAGED_RANGE_PAGES;

/// The pages of credit an object's holders are granted at once, and kept
/// topped up to: room the volume holds and no holder may have dirtied yet.
pub const BULK: u64 = 64;

/// A pager's calls on its objects, by id, as `toyos::paged::Pager` makes them.
pub trait Pager {
    fn object(&self, size: u64) -> Result<u64, Refused>;
    fn anonymous(&self, size: u64) -> Result<u64, Refused>;
    fn room(&self) -> Result<u64, Refused>;
    fn read(&self, id: u64, out: Bytes<'_>, offset: u64) -> Result<usize, Refused>;
    fn supply(&self, id: u64, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused>;
    fn write(&self, id: u64, offset: u64, bytes: &[u8]) -> Result<usize, Refused>;
    fn hold(&self, id: u64) -> Result<u64, Refused>;
    fn unhold(&self, id: u64) -> Result<(), Refused>;
    fn withdraw(&self, id: u64, first: u64, count: u64) -> Result<u64, Refused>;
    fn fail(&self, id: u64, first: u64, count: u64) -> Result<(), Refused>;
    fn resize(&self, id: u64, size: u64) -> Result<(), Refused>;
    fn writeback_begin(&self, id: u64, first: u64, count: u64) -> Result<[u64; 8], Refused>;
    fn writeback_end(&self, id: u64, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused>;
    fn release(&self, id: u64) -> Result<(), Refused>;
    fn grant(&self, id: u64, pages: u64) -> Result<u64, Refused>;
    fn take_back(&self, id: u64) -> Result<u64, Refused>;
    fn set_record(&self, id: u64, record: Option<&[u8; PAGED_RECORD]>, mtime: Option<u64>) -> Result<(), Refused>;
    fn status(&self, id: u64) -> Result<PagedStatus, Refused>;
    fn take_dirtied(&self, id: u64) -> Result<Option<(u64, u64)>, Refused>;
}

impl Pager for toyos::paged::Pager {
    fn object(&self, size: u64) -> Result<u64, Refused> {
        toyos::paged::Pager::object(self, size)
    }
    fn anonymous(&self, size: u64) -> Result<u64, Refused> {
        toyos::paged::Pager::anonymous(self, size)
    }
    fn room(&self) -> Result<u64, Refused> {
        toyos::paged::Pager::room(self)
    }
    fn read(&self, id: u64, out: Bytes<'_>, offset: u64) -> Result<usize, Refused> {
        toyos::paged::Pager::read(self, id, out, offset)
    }
    fn supply(&self, id: u64, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused> {
        toyos::paged::Pager::supply(self, id, offset, bytes, dirty)
    }
    fn write(&self, id: u64, offset: u64, bytes: &[u8]) -> Result<usize, Refused> {
        toyos::paged::Pager::write(self, id, offset, bytes)
    }
    fn hold(&self, id: u64) -> Result<u64, Refused> {
        toyos::paged::Pager::hold(self, id)
    }
    fn unhold(&self, id: u64) -> Result<(), Refused> {
        toyos::paged::Pager::unhold(self, id)
    }
    fn withdraw(&self, id: u64, first: u64, count: u64) -> Result<u64, Refused> {
        toyos::paged::Pager::withdraw(self, id, first, count)
    }
    fn fail(&self, id: u64, first: u64, count: u64) -> Result<(), Refused> {
        toyos::paged::Pager::fail(self, id, first, count)
    }
    fn resize(&self, id: u64, size: u64) -> Result<(), Refused> {
        toyos::paged::Pager::resize(self, id, size)
    }
    fn writeback_begin(&self, id: u64, first: u64, count: u64) -> Result<[u64; 8], Refused> {
        toyos::paged::Pager::writeback_begin(self, id, first, count)
    }
    fn writeback_end(&self, id: u64, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused> {
        toyos::paged::Pager::writeback_end(self, id, first, count, written)
    }
    fn release(&self, id: u64) -> Result<(), Refused> {
        toyos::paged::Pager::release(self, id)
    }
    fn grant(&self, id: u64, pages: u64) -> Result<u64, Refused> {
        toyos::paged::Pager::grant(self, id, pages)
    }
    fn take_back(&self, id: u64) -> Result<u64, Refused> {
        toyos::paged::Pager::take_back(self, id)
    }
    fn set_record(&self, id: u64, record: Option<&[u8; PAGED_RECORD]>, mtime: Option<u64>) -> Result<(), Refused> {
        toyos::paged::Pager::set_record(self, id, record, mtime)
    }
    fn status(&self, id: u64) -> Result<PagedStatus, Refused> {
        toyos::paged::Pager::status(self, id)
    }
    fn take_dirtied(&self, id: u64) -> Result<Option<(u64, u64)>, Refused> {
        toyos::paged::Pager::take_dirtied(self, id)
    }
}

/// What a client is answered for a refusal of the object's.
fn word(why: Refused) -> Error {
    println!("fileserver: a paged object refused: {why:?}");
    match why {
        Refused::NoRoom | Refused::DirtyFull => Error::NoSpace,
        Refused::Absent | Refused::Gone | Refused::Kernel(_) => Error::Io,
    }
}

struct File {
    /// Its object's id on the pager.
    object: u64,
    node: Node,
    /// Descriptions open on it.
    opens: u32,
    /// A holder handle went out and no notice has said the last is gone.
    held: bool,
    /// The pages written since their last write-back: a range, `lo..hi`.
    dirty: Option<(u64, u64)>,
    /// A holder dirtied pages the object has not told the range of yet.
    holder_dirty: bool,
    /// Credit is granted, and kept topped up, for a holder minted to write.
    granting: bool,
    /// A holder may have written it: its size and time are the object's,
    /// past the volume's, until the volume takes them with no holder left.
    moved: bool,
    /// What its holders read of it beside its size and time.
    record: Record,
    /// When it was last used, by [`Files::tick`].
    used: u64,
    anonymous: bool,
}

impl File {
    fn dirtied(&mut self, first: u64, end: u64) {
        if self.anonymous { return; }
        let (lo, hi) = self.dirty.unwrap_or((first, end));
        self.dirty = Some((lo.min(first), hi.max(end)));
    }

    fn droppable(&self) -> bool {
        self.opens == 0 && !self.held && self.dirty.is_none() && !self.holder_dirty
    }

    /// Whether pages may wait for its write-back: ones it knows, or ones a
    /// holder that writes may have dirtied without a notice yet.
    fn may_owe(&self) -> bool {
        !self.anonymous && (self.dirty.is_some() || self.holder_dirty || self.granting)
    }
}

pub struct Files<P: Pager> {
    pager: Option<P>,
    /// The most files kept on a volume whose files can be let go; `None` for
    /// a memory volume, whose objects are its files.
    keeps: Option<usize>,
    files: BTreeMap<u64, File>,
    /// Each object's file, by the object's id, which the notices name.
    by_object: BTreeMap<u64, u64>,
    tick: u64,
    /// A supply's or a write-back's pages, kept so one allocates nothing.
    buf: Vec<u8>,
}

impl<P: Pager> Files<P> {
    pub fn new(pager: Option<P>, keeps: Option<usize>) -> Self {
        Self { pager, keeps, files: BTreeMap::new(), by_object: BTreeMap::new(), tick: 0, buf: Vec::new() }
    }

    /// The pager, which reads ready while a notice is owed.
    pub fn pager(&self) -> Option<&P> {
        self.pager.as_ref()
    }

    pub fn room(&self, volume: &mut dyn Volume) -> Result<toyos::fs::Room, Error> {
        if volume.memory() {
            Ok(toyos::fs::Room { free: self.pg().room().map_err(word)?.saturating_mul(PAGE), unit: PAGE })
        } else {
            volume.room()
        }
    }

    fn pg(&self) -> &P {
        self.pager.as_ref().expect("fileserver: a paged file on a server with no pager")
    }

    /// The object id of the file `identity` names, for a holder of it.
    pub fn object(&self, identity: u64) -> Option<u64> {
        self.files.get(&identity).map(|f| f.object)
    }

    /// A holder of `identity`'s object went out, minted to write where
    /// `writes`: its credit is granted where the volume holds room for it.
    pub fn held(&mut self, volume: &mut dyn Volume, identity: u64, writes: bool) {
        let Some(f) = self.files.get_mut(&identity) else { return };
        f.held = true;
        f.moved |= writes;
        if writes && !f.anonymous && !f.granting {
            f.granting = true;
            self.top_up(volume, identity);
        }
    }

    /// Whether a holder of `identity`'s object could write it: an anonymous
    /// object, or one on a volume that holds room for its holders' writes.
    /// Elsewhere every write comes here.
    pub fn holders_write(&self, volume: &dyn Volume, identity: u64) -> bool {
        self.files.get(&identity).is_some_and(|f| f.anonymous || volume.grants())
    }

    /// `f` with `identity`'s writes held: given the size the other writers
    /// left, what it does is one write, truncation or read whole to them. The
    /// hold is given back whatever `f` answers.
    fn under_hold<R>(&mut self, identity: u64, f: impl FnOnce(&mut Self, u64) -> Result<R, Error>) -> Result<R, Error> {
        let id = self.files.get(&identity).ok_or(Error::Stale)?.object;
        let size = self.pg().hold(id).map_err(word)?;
        let answer = f(self, size);
        if let Err(why) = self.pg().unhold(id) {
            println!("fileserver: file {identity}'s writes would not be given back ({why:?})");
        }
        answer
    }

    /// The credit of `identity`'s holders made up to [`BULK`] again, room
    /// for it held first. A volume that holds no more room leaves it short,
    /// and holders ask this server, which answers the write.
    fn top_up(&mut self, volume: &mut dyn Volume, identity: u64) {
        let Some(f) = self.files.get(&identity).filter(|f| f.granting) else { return };
        let (object, node) = (f.object, f.node);
        let Ok(status) = self.pg().status(object) else { return };
        let want = BULK.saturating_sub(status.credit);
        if want == 0 || volume.grant(node, want).is_err() {
            return;
        }
        if let Err(why) = self.pg().grant(object, want) {
            println!("fileserver: file {identity}'s credit was not granted ({why:?})");
            volume.ungrant(node, want);
        }
    }

    /// What `identity` is, its size and time its object's while a holder may
    /// have moved them past the volume's.
    pub fn meta(&self, identity: u64, meta: Meta) -> Meta {
        let Some(f) = self.files.get(&identity).filter(|f| f.moved && meta.kind == Kind::File) else { return meta };
        match self.pg().status(f.object) {
            Ok(status) => Meta { size: status.size, mtime: status.mtime, ..meta },
            Err(why) => {
                println!("fileserver: file {identity}'s object would not say its size ({why:?})");
                meta
            }
        }
    }

    /// `identity`'s object stamped as last written at `mtime`, where it has one.
    pub fn stamp(&self, identity: u64, mtime: u64) -> Result<(), Error> {
        match self.files.get(&identity) {
            Some(f) => self.pg().set_record(f.object, None, Some(mtime)).map_err(word),
            None => Ok(()),
        }
    }

    /// One line of what is held, for the server's.
    pub fn describe(&self) -> String {
        let dirty = self.files.values().filter(|f| f.dirty.is_some() || f.holder_dirty).count();
        format!("{} paged files, {dirty} dirty", self.files.len())
    }

    fn touch(&mut self, identity: u64) -> Option<&mut File> {
        self.tick += 1;
        let tick = self.tick;
        let f = self.files.get_mut(&identity)?;
        f.used = tick;
        Some(f)
    }

    /// After an open of `identity` at `node` that truncated: its object
    /// holds no page of before, and the volume's time. The volume went first,
    /// so a refused open changed nothing, and nothing writes back between the
    /// two.
    pub fn truncated(&mut self, volume: &mut dyn Volume, identity: u64, node: Node) -> Result<(), Error> {
        if !self.files.contains_key(&identity) {
            return Ok(());
        }
        self.under_hold(identity, |files, _| {
            let f = files.files.get_mut(&identity).expect("found");
            f.dirty = None;
            let id = f.object;
            files.pg().resize(id, 0).map_err(word)?;
            files.stamp(identity, volume.node_meta(node)?.mtime)
        })
    }

    /// A description opened `identity` at `node`, which the volume holds once
    /// more for it, its holders to read `record` of it. The object's file
    /// holds it once for good, so a second hold goes back at once.
    pub fn opened(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, record: Record) -> Result<(), Error> {
        assert!(!volume.memory() || self.pager.is_some(), "memory files require a pager");
        if self.pager.is_none() {
            return Ok(());
        }
        match self.files.get(&identity).map(|f| f.node) {
            Some(held) if held == node => {
                self.touch(identity).expect("just found").opens += 1;
                return volume.close(node);
            }
            // The node it held is no longer the file's: it went Stale.
            Some(_) => self.forget(volume, identity),
            None => {}
        }
        let meta = volume.node_meta(node)?;
        // A file nobody holds goes to make room for another's: at the count
        // kept, or where the pager's job is charged no more.
        if self.keeps.is_some_and(|keeps| self.files.len() >= keeps) && !self.drop_oldest(volume, identity) {
            println!("fileserver: every file kept is held; file {identity} is refused");
            return Err(Error::NoSpace);
        }
        let object = loop {
            let created = if volume.memory() { self.pg().anonymous(meta.size) } else { self.pg().object(meta.size) };
            match created {
                Ok(object) => break object,
                Err(Refused::NoRoom) if self.drop_oldest(volume, identity) => {}
                Err(why) => return Err(word(why)),
            }
        };
        let file = File {
            object,
            node,
            opens: 1,
            held: false,
            dirty: None,
            holder_dirty: false,
            granting: false,
            moved: false,
            record,
            used: 0,
            anonymous: volume.memory(),
        };
        self.files.insert(identity, file);
        self.by_object.insert(object, identity);
        self.touch(identity);
        if let Err(why) = self.pg().set_record(object, Some(&record.encode()), Some(meta.mtime)) {
            self.forget(volume, identity);
            return Err(word(why));
        }
        Ok(())
    }

    /// A description of `identity` at `node` closed.
    pub fn closed(&mut self, volume: &mut dyn Volume, identity: u64, node: Node) -> Result<(), Error> {
        match self.files.get_mut(&identity) {
            Some(f) => {
                f.opens = f.opens.saturating_sub(1);
                self.collect_unlinked(volume, identity)?;
                Ok(())
            }
            None if self.pager.is_none() => volume.close(node),
            None => Ok(()),
        }
    }

    /// A removed name does not invalidate an inode the volume still holds:
    /// its holders read that it has none.
    pub fn unlinked(&mut self, volume: &mut dyn Volume, identity: u64) -> Result<(), Error> {
        let Some(f) = self.files.get_mut(&identity) else { return Ok(()) };
        f.record.nlink = 0;
        let (object, record) = (f.object, f.record.encode());
        self.pg().set_record(object, Some(&record), None).map_err(word)?;
        let f = &self.files[&identity];
        match volume.node_meta(f.node) {
            Ok(_) => self.collect_unlinked(volume, identity),
            Err(Error::Stale) => { self.forget(volume, identity); Ok(()) }
            Err(error) => Err(error),
        }
    }

    fn collect_unlinked(&mut self, volume: &mut dyn Volume, identity: u64) -> Result<(), Error> {
        let Some(f) = self.files.get(&identity) else { return Ok(()) };
        if f.opens == 0 && !f.held && !volume.linked(f.node)? {
            self.forget(volume, identity);
        }
        Ok(())
    }

    /// `identity`'s file is gone from the volume: its object goes, its
    /// holders answer `Gone`, and its node is let go.
    pub fn forget(&mut self, volume: &mut dyn Volume, identity: u64) {
        if let Some(f) = self.files.remove(&identity) {
            self.by_object.remove(&f.object);
            if let Err(why) = self.pg().release(f.object) {
                println!("fileserver: file {identity}'s object would not be released ({why:?})");
            }
            if let Err(e) = volume.close(f.node) {
                println!("fileserver: a forgotten file's node {} would not close ({e:?})", f.node);
            }
        }
    }

    /// Up to `out.len()` bytes of `node`'s file at `offset`, read whole
    /// against every write.
    pub fn read(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, offset: u64, out: Bytes<'_>) -> Result<usize, Error> {
        if self.pager.is_none() || !self.files.contains_key(&identity) {
            return volume.read(node, offset, &mut Window(out));
        }
        self.under_hold(identity, |files, size| files.read_held(volume, identity, offset, size, out))
    }

    /// [`Self::read`] of a file `size` bytes long, its writes held.
    fn read_held(&mut self, volume: &mut dyn Volume, identity: u64, offset: u64, size: u64, out: Bytes<'_>) -> Result<usize, Error> {
        if offset >= size {
            return Ok(0);
        }
        let want = out.len().min((size - offset) as usize);
        let mut done = 0;
        while done < want {
            let at = offset + done as u64;
            let id = self.touch(identity).expect("checked").object;
            match self.pg().read(id, out.sub(done, want - done), at) {
                Ok(n) if n > 0 => {
                    done += n;
                    continue;
                }
                Ok(_) | Err(Refused::Absent) => {}
                Err(why) => return Err(word(why)),
            }
            let first = at / PAGE;
            let end = (offset + want as u64).div_ceil(PAGE);
            if self.fill(volume, identity, first, (end - first).min(RANGE))? == 0 {
                println!("fileserver: page {first} of file {identity} would not stay supplied");
                return Err(Error::Io);
            }
        }
        Ok(done)
    }

    /// Supply `count` pages from `first` off the volume, but for those past
    /// the size: how many the object took or held already.
    fn fill(&mut self, volume: &mut dyn Volume, identity: u64, first: u64, count: u64) -> Result<u64, Error> {
        let Some(f) = self.files.get(&identity) else { return Ok(0) };
        let node = f.node;
        // The volume reads what lies past its own size as the hole it is.
        let size = self.meta(identity, volume.node_meta(node)?).size;
        let end = (first + count).min(size.div_ceil(PAGE));
        let mut buf = std::mem::take(&mut self.buf);
        let mut at = first;
        let mut filled = Ok(0);
        while at < end {
            let pages = (end - at).min(RANGE);
            let bytes = (pages * PAGE) as usize;
            buf.clear();
            buf.resize(bytes, 0);
            if let Err(e) = volume.read(node, at * PAGE, &mut Buf(&mut buf)) {
                filled = Err(e);
                break;
            }
            match self.supplied(volume, identity, at, pages, &buf, false) {
                Ok(_) => {}
                Err(e) => {
                    filled = Err(e);
                    break;
                }
            }
            at += pages;
        }
        self.buf = buf;
        filled.map(|_: u64| at - first)
    }

    /// `bytes` supplied at page `first`, room made where the pager has none
    /// and a write-back where its dirty ceiling is reached: the pages installed.
    fn supplied(&mut self, volume: &mut dyn Volume, identity: u64, first: u64, pages: u64, bytes: &[u8], dirty: bool) -> Result<u64, Error> {
        loop {
            let id = self.files.get(&identity).ok_or(Error::Stale)?.object;
            match self.pg().supply(id, first * PAGE, bytes, dirty) {
                Ok(n) => return Ok(n),
                Err(Refused::NoRoom) if self.make_room(volume, identity, first, pages)? => {}
                Err(Refused::DirtyFull) if self.write_back_some(volume) => {}
                Err(why) => return Err(word(why)),
            }
        }
    }

    /// Room for `count` pages from `first` of `identity`'s file: a file
    /// nobody holds dropped, else another's clean pages withdrawn, else this
    /// one's outside the range, else the dirty written back. Whether any went.
    fn make_room(&mut self, volume: &mut dyn Volume, identity: u64, first: u64, count: u64) -> Result<bool, Error> {
        if volume.memory() { return Ok(false); }
        if self.drop_oldest(volume, identity) {
            return Ok(true);
        }
        for id in self.oldest(identity, &|_| true) {
            let object = self.files[&id].object;
            if self.pg().withdraw(object, 0, u64::MAX).map_err(word)? > 0 {
                return Ok(true);
            }
        }
        let own = self.files[&identity].object;
        if self.pg().withdraw(own, 0, first).map_err(word)? + self.pg().withdraw(own, first + count, u64::MAX).map_err(word)? > 0 {
            return Ok(true);
        }
        Ok(self.write_back_some(volume))
    }

    /// The files but `identity` that `pick` takes, the least recently used first.
    fn oldest(&self, identity: u64, pick: &dyn Fn(&File) -> bool) -> Vec<u64> {
        let mut by_age: Vec<(u64, u64)> =
            self.files.iter().filter(|(id, f)| **id != identity && pick(f)).map(|(id, f)| (f.used, *id)).collect();
        by_age.sort_unstable();
        by_age.into_iter().map(|(_, id)| id).collect()
    }

    /// The oldest file nobody holds, open or dirties, but `identity`,
    /// forgotten: whether there was one.
    fn drop_oldest(&mut self, volume: &mut dyn Volume, identity: u64) -> bool {
        if volume.memory() { return false; }
        match self.oldest(identity, &File::droppable).first() {
            Some(&id) => {
                self.forget(volume, id);
                true
            }
            None => false,
        }
    }

    /// Every file that may owe a write-back written back, for room, the pages
    /// holders dirtied among them: whether any page was.
    fn write_back_some(&mut self, volume: &mut dyn Volume) -> bool {
        let dirty: Vec<u64> = self.files.iter().filter(|(_, f)| f.may_owe()).map(|(id, _)| *id).collect();
        let mut cleaned = false;
        for id in dirty {
            match self.write_back(volume, id) {
                Ok(()) => cleaned = true,
                Err(e) => println!("fileserver: file {id}'s write-back for room failed ({e:?})"),
            }
        }
        cleaned
    }

    /// `data` written to `node`'s file at `at`, or at its end where `None`,
    /// one write whole to every other writer and reader of it: the offset
    /// after the bytes.
    pub fn write(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, at: Option<u64>, data: &[u8]) -> Result<u64, Error> {
        if self.pager.is_none() || !self.files.contains_key(&identity) {
            let offset = match at {
                Some(at) => at,
                None => volume.node_meta(node)?.size,
            };
            let end = crate::bounds::in_a_file(offset, data.len() as u64)?;
            if !data.is_empty() {
                volume.write(node, offset, data)?;
            }
            return Ok(end);
        }
        // Stamped inside the hold, so no holder states the new size with the old time.
        let end = self.under_hold(identity, |files, size| {
            let offset = at.unwrap_or(size);
            let end = crate::bounds::in_a_file(offset, data.len() as u64)?;
            if !data.is_empty() {
                files.write_held(volume, identity, node, size, offset, data)?;
                files.stamp(identity, volume.node_meta(node)?.mtime)?;
            }
            Ok(end)
        })?;
        if !data.is_empty() {
            // A holder asks this server once its credit is spent.
            self.top_up(volume, identity);
        }
        Ok(end)
    }

    /// [`Self::write`] of `data` at `offset` into a file `old` bytes long,
    /// its writes held.
    fn write_held(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, old: u64, offset: u64, data: &[u8]) -> Result<(), Error> {
        let reserved = volume.reserve(node, offset, data.len() as u64)?;
        self.touch(identity);
        let mut done = 0;
        let writing = Writing { identity, node, reserved, offset, data };
        let written = self.put(volume, &writing, &mut done);
        if let Err(e) = &written {
            // What was reserved and not put would read as the blocks' old
            // bytes. No other writer moved the size since the hold, so what
            // goes is this write's own.
            let landed = if done > 0 { offset + done as u64 } else { 0 };
            let cut = old.max(landed);
            let id = self.files.get(&identity).ok_or(Error::Stale)?.object;
            let resized = self.pg().resize(id, cut).map_err(word).and(volume.unreserve(node, reserved, landed));
            if let Err(again) = resized {
                println!("fileserver: a refused write of file {identity} was not cut back to {cut} ({again:?}), after {e:?}");
            }
        }
        written
    }

    /// The bytes of a write into the object, each absent page supplied dirty,
    /// counting into `done` what landed.
    fn put(&mut self, volume: &mut dyn Volume, w: &Writing<'_>, done: &mut usize) -> Result<(), Error> {
        let Writing { identity, node, reserved, offset, data } = *w;
        let end = offset + data.len() as u64;
        while *done < data.len() {
            let at = offset + *done as u64;
            let id = self.files.get(&identity).ok_or(Error::Stale)?.object;
            match self.pg().write(id, at, &data[*done..]) {
                Ok(n) => {
                    self.landed(volume, w, at, n);
                    *done += n;
                    continue;
                }
                Err(Refused::Absent) => {}
                Err(Refused::DirtyFull) if self.write_back_some(volume) => continue,
                Err(why) => return Err(word(why)),
            }
            let page = at / PAGE;
            let whole = ((data.len() - *done) as u64 / PAGE).min(RANGE);
            if at.is_multiple_of(PAGE) && whole > 0 {
                let bytes = &data[*done..*done + (whole * PAGE) as usize];
                // Dirty supply stops at the first present page. The next
                // write handles it, so every reported byte is a prefix.
                let taken = self.supplied(volume, identity, page, whole, bytes, true)?;
                let len = (taken * PAGE) as usize;
                self.landed(volume, w, at, len);
                *done += len;
                continue;
            }
            // A page in part: what it held below the old end, zeros past the
            // old contents, then the write's.
            let start = page * PAGE;
            let below = reserved.size.saturating_sub(start).min(PAGE) as usize;
            let held = reserved.contents().saturating_sub(start).min(PAGE) as usize;
            let upto = end.min(start + PAGE);
            let mut buf = std::mem::take(&mut self.buf);
            buf.clear();
            buf.resize(PAGE as usize, 0);
            let read = volume.read(node, start, &mut Buf(&mut buf[..held]));
            let supplied = read.and_then(|_| {
                buf[(at - start) as usize..(upto - start) as usize].copy_from_slice(&data[*done..*done + (upto - at) as usize]);
                let len = below.max((upto - start) as usize);
                self.supplied(volume, identity, page, 1, &buf[..len], true)
            });
            self.buf = buf;
            if supplied? == 1 {
                let len = (upto - at) as usize;
                self.landed(volume, w, at, len);
                *done += len;
            }
        }
        Ok(())
    }

    /// `len` bytes at `at` landed dirty: the next write-back takes them, a
    /// write-back the same write makes room with included.
    fn landed(&mut self, volume: &mut dyn Volume, w: &Writing<'_>, at: u64, len: usize) {
        if len > 0 {
            volume.landed(w.node, w.reserved, at, len);
            let f = self.files.get_mut(&w.identity).expect("a file being written");
            f.dirtied(at / PAGE, (at + len as u64).div_ceil(PAGE));
        }
    }

    /// `node`'s file cut or grown to `size`: the volume first, so a refusal
    /// changes nothing, then the object, before anything can write a page
    /// past the size back.
    pub fn truncate(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, size: u64) -> Result<(), Error> {
        if !self.files.contains_key(&identity) {
            return volume.truncate(node, size);
        }
        self.under_hold(identity, |files, now| {
            let f = files.files.get_mut(&identity).expect("found");
            let id = f.object;
            let mtime = files.pg().status(id).map_err(word)?.mtime;
            volume.adopt(node, now, mtime);
            volume.truncate(node, size)?;
            let f = files.files.get_mut(&identity).expect("found");
            if let Some((lo, hi)) = f.dirty {
                let hi = hi.min(size.div_ceil(PAGE));
                f.dirty = (lo < hi).then_some((lo, hi));
            }
            files.pg().resize(id, size).map_err(word)?;
            files.stamp(identity, volume.node_meta(node)?.mtime)
        })
    }

    /// `identity`'s dirty pages written to the volume. A page dirtied again
    /// meanwhile, or whose write was refused, stays dirty.
    fn write_back(&mut self, volume: &mut dyn Volume, identity: u64) -> Result<(), Error> {
        let Some(f) = self.files.get(&identity).filter(|f| f.may_owe()) else { return Ok(()) };
        let (object, node) = (f.object, f.node);
        // The pages holders dirtied first, then the size: a page whose write
        // has not grown the size yet waits for the next write-back.
        let taken = self.pg().take_dirtied(object).map_err(word)?;
        let status = self.pg().status(object).map_err(word)?;
        volume.adopt(node, status.size, status.mtime);
        let f = self.files.get_mut(&identity).expect("found");
        f.holder_dirty = false;
        if let Some((first, count)) = taken {
            f.dirtied(first, first + count);
        }
        let Some((lo, hi)) = f.dirty else { return Ok(()) };
        let reach = status.size.div_ceil(PAGE);
        let mut kept: Option<(u64, u64)> = (hi > reach).then_some((reach.max(lo), hi));
        let hi = hi.min(reach);
        let size = status.size;
        let mut buf = std::mem::take(&mut self.buf);
        let mut failed = Ok(());
        let mut first = lo;
        while first < hi {
            let count = (hi - first).min(RANGE);
            let object = self.files[&identity].object;
            let pager = self.pager.as_ref().expect("paged");
            let begun = pager.writeback_begin(object, first, count).map_err(word)?;
            let mut written = Ok(());
            let mut complete = count;
            for (start, pages) in runs(&begun, count) {
                let at = (first + start) * PAGE;
                let len = (pages * PAGE).min(size - at) as usize;
                buf.clear();
                buf.resize(len, 0);
                // SAFETY: `buf` is this process's, reached only through these
                // bytes while they live.
                let out = unsafe { Bytes::from_raw(buf.as_mut_ptr(), len) };
                let result = match pager.read(object, out, at) {
                    Ok(n) if n == len => volume.write_back(node, at, &buf),
                    Ok(n) => {
                        println!("fileserver: file {identity}'s pages under write-back at {at} read {n} of {len} bytes");
                        Err((0, Error::Io))
                    }
                    Err(why) => Err((0, word(why))),
                };
                if let Err((done, error)) = result {
                    assert!(done < len && (done as u64).is_multiple_of(PAGE), "write-back refusal reports a whole-page prefix");
                    complete = start + done as u64 / PAGE;
                    written = Err(error);
                    break;
                }
            }
            for (at, pages, accepted) in [(first, complete, true), (first + complete, count - complete, false)] {
                if pages == 0 { continue; }
                let still = pager.writeback_end(object, at, pages, accepted).map_err(word)?;
                for (start, pages) in runs(&still, pages) {
                    let (a, b) = (at + start, at + start + pages);
                    let (x, y) = kept.unwrap_or((a, b));
                    kept = Some((x.min(a), y.max(b)));
                }
            }
            failed = failed.and(written);
            first += count;
        }
        self.buf = buf;
        let f = self.files.get_mut(&identity).expect("found");
        f.dirty = kept;
        // Nothing waits for its write-back and no holder can dirty a page:
        // the room it holds for one goes back.
        if kept.is_none() && !f.held && !f.holder_dirty {
            volume.ungrant(node, u64::MAX);
        }
        self.settle(volume, identity);
        failed
    }

    /// Where no holder is left to write `identity` and no page it wrote
    /// waits for a write-back, the volume takes the object's size and time,
    /// and they are the volume's again.
    fn settle(&mut self, volume: &mut dyn Volume, identity: u64) {
        let Some(f) = self.files.get(&identity) else { return };
        if !f.moved || f.held || f.dirty.is_some() || f.holder_dirty {
            return;
        }
        let (object, node) = (f.object, f.node);
        match self.pg().status(object) {
            Ok(status) => {
                volume.adopt(node, status.size, status.mtime);
                self.files.get_mut(&identity).expect("found").moved = false;
            }
            Err(why) => println!("fileserver: file {identity}'s object would not say its size ({why:?})"),
        }
    }

    /// Every dirty page to the volume, then the volume synced: the files
    /// whose entries or pages were refused, with why.
    pub fn sync(&mut self, volume: &mut dyn Volume) -> Result<Vec<(Node, Error)>, Error> {
        let dirty: Vec<u64> = self.files.iter().filter(|(_, f)| f.may_owe()).map(|(id, _)| *id).collect();
        let mut refused = Vec::new();
        for id in dirty {
            if let Err(e) = self.write_back(volume, id) {
                refused.push((self.files[&id].node, e));
            }
        }
        let mut unwritten = volume.sync()?;
        for (node, error) in refused {
            match unwritten.iter_mut().find(|(held, _)| *held == node) {
                Some((_, why)) => *why = error,
                None => unwritten.push((node, error)),
            }
        }
        Ok(unwritten)
    }

    /// An fsync: `file`'s pages to the volume, and every file's the volume's
    /// commit carries, then the commit. A carried file whose pages would not
    /// go refuses it, since its entry would name blocks without its bytes.
    pub fn fsync(&mut self, volume: &mut dyn Volume, file: Option<(u64, Node)>) -> Result<(), Error> {
        let named = file.map(|(identity, _)| identity);
        let owed: Vec<u64> = self
            .files
            .iter()
            .filter(|(id, f)| f.may_owe() && (Some(**id) == named || volume.carried(f.node)))
            .map(|(id, _)| *id)
            .collect();
        for id in owed {
            if let Err(e) = self.write_back(volume, id) {
                if Some(id) != named {
                    println!("fileserver: file {id}'s pages would not go to the volume ({e:?}); an fsync of another is refused");
                }
                return Err(e);
            }
        }
        volume.commit(file.map(|(_, node)| node))
    }

    /// Whether a change is not yet durable: a dirty page, or an entry the
    /// volume owes ([`Volume::owes`]).
    pub fn owes(&self, volume: &dyn Volume) -> bool {
        self.files.values().any(|f| f.dirty.is_some() || f.holder_dirty) || volume.owes()
    }

    /// What the pager's objects asked: whether a write-back is owed.
    pub fn notice(&mut self, volume: &mut dyn Volume, notice: Notice) -> bool {
        match notice {
            Notice::Requested { object, first, count } => {
                let Some(&identity) = self.by_object.get(&object) else { return false };
                let count = count.min(RANGE);
                // A reader waits for its pages until they are supplied or
                // failed: a range the volume will not read, or holds none of,
                // is failed.
                let why = match self.fill(volume, identity, first, count) {
                    Ok(0) => "the volume holds none of them".to_string(),
                    Ok(_) => return false,
                    Err(e) => format!("{e:?}"),
                };
                println!("fileserver: pages {first}+{count} of file {identity} were not supplied ({why}); failing them");
                if let Err(why) = self.pg().fail(object, first, count) {
                    println!("fileserver: and the failure was refused ({why:?})");
                }
                false
            }
            Notice::NoHolders { object } => {
                let Some(&identity) = self.by_object.get(&object) else { return false };
                // No holder writes again: what one dirtied is this server's
                // to know now, and the credit it did not spend is room again.
                let taken = self.pg().take_dirtied(object);
                let credit = self.pg().take_back(object);
                let f = self.files.get_mut(&identity).expect("an indexed object has a file");
                f.held = false;
                if let Ok(Some((first, count))) = taken {
                    f.dirtied(first, first + count);
                    f.holder_dirty = false;
                }
                if std::mem::take(&mut f.granting) && let Ok(credit) = credit {
                    volume.ungrant(f.node, credit);
                }
                let owed = f.dirty.is_some() || f.holder_dirty;
                self.settle(volume, identity);
                if let Err(e) = self.collect_unlinked(volume, identity) {
                    println!("fileserver: unlinked file {identity} would not be collected ({e:?})");
                }
                owed
            }
            Notice::Dirtied { object } => {
                let Some(&identity) = self.by_object.get(&object) else { return false };
                let f = self.files.get_mut(&identity).expect("an indexed object has a file");
                f.holder_dirty = !f.anonymous;
                f.holder_dirty
            }
        }
    }
}

/// A write under way and the capacity held until its bytes land.
#[derive(Clone, Copy)]
struct Writing<'a> {
    identity: u64,
    node: Node,
    reserved: Reserved,
    offset: u64,
    data: &'a [u8],
}

/// The runs of set bits among the first `count` of `bits`: where each starts
/// and how long it is.
fn runs(bits: &[u64; 8], count: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
    let set = move |k: u64| bits[(k / 64) as usize] & (1 << (k % 64)) != 0;
    let mut k = 0;
    std::iter::from_fn(move || {
        while k < count && !set(k) {
            k += 1;
        }
        if k == count {
            return None;
        }
        let start = k;
        while k < count && set(k) {
            k += 1;
        }
        Some((start, k - start))
    })
}

#[cfg(test)]
mod tests;
