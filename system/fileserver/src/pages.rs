//! A file's bytes in memory: one paged object of the kernel's per file
//! ([`toyos::paged`]), which its holders read without asking this server and
//! which is the only copy of the file's data the server keeps. This moves
//! pages between the object and the volume: a page a reader asks for is read
//! off the volume and supplied, a write lands dirty in the object, and a sync
//! writes the dirty pages back before the volume's own sync.
//!
//! **An object per file, by its volume identity**, which the notices name it
//! by. It holds the volume's node for its life, so a dirty file stays open
//! on the volume whoever closed it, and outlives its last close for its pages'
//! sake until room is wanted: then a file nobody holds and nothing dirties
//! goes first, oldest first.
//!
//! **A write is one write of the object's**, however many calls it takes:
//! a call that stops short leaves it under way until its end, so no holder
//! reads the file half written (`toyos::paged::Pager::end_write`).
//!
//! **The volume's size and mtime are the object's**, which its holders
//! `fstat` without a request: each call that moves the volume's mtime hands
//! the object the volume's own value, at its resolution. A write reserves on the volume
//! first, so the volume's size runs past the object's only for the bytes the
//! write then puts in before it returns; nothing reads the volume for them
//! meanwhile, and a write refused part way is cut back to what it put.
//!
//! **A server with no pager** (ROOT's, whose files are immutable) reads and
//! writes the volume directly, and each open holds its own node.

use std::collections::BTreeMap;

use toyos::fs::Error;
use toyos::paged::{Notice, Refused, PAGE, PAGED_RANGE_PAGES};
use toyos::shm::Bytes;

use crate::volume::{Buf, Node, Volume, Window, Writes};

/// The most pages one supply or write-back carries.
const RANGE: u64 = PAGED_RANGE_PAGES;

/// A pager's calls on its objects, by id, as `toyos::paged::Pager` makes them.
pub trait Pager {
    fn object(&self, size: u64, mtime: u64) -> Result<u64, Refused>;
    fn read(&self, id: u64, out: Bytes<'_>, offset: u64) -> Result<usize, Refused>;
    fn supply(&self, id: u64, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused>;
    fn write(&self, id: u64, offset: u64, bytes: &[u8], mtime: u64) -> Result<usize, Refused>;
    fn end_write(&self, id: u64) -> Result<(), Refused>;
    fn withdraw(&self, id: u64, first: u64, count: u64) -> Result<u64, Refused>;
    fn fail(&self, id: u64, first: u64, count: u64) -> Result<(), Refused>;
    fn resize(&self, id: u64, size: u64, mtime: u64) -> Result<(), Refused>;
    fn stamp(&self, id: u64, mtime: u64) -> Result<(), Refused>;
    fn writeback_begin(&self, id: u64, first: u64, count: u64) -> Result<[u64; 8], Refused>;
    fn writeback_end(&self, id: u64, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused>;
    fn release(&self, id: u64) -> Result<(), Refused>;
}

impl Pager for toyos::paged::Pager {
    fn object(&self, size: u64, mtime: u64) -> Result<u64, Refused> {
        toyos::paged::Pager::object(self, size, mtime)
    }
    fn read(&self, id: u64, out: Bytes<'_>, offset: u64) -> Result<usize, Refused> {
        toyos::paged::Pager::read(self, id, out, offset)
    }
    fn supply(&self, id: u64, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused> {
        toyos::paged::Pager::supply(self, id, offset, bytes, dirty)
    }
    fn write(&self, id: u64, offset: u64, bytes: &[u8], mtime: u64) -> Result<usize, Refused> {
        toyos::paged::Pager::write(self, id, offset, bytes, mtime)
    }
    fn end_write(&self, id: u64) -> Result<(), Refused> {
        toyos::paged::Pager::end_write(self, id)
    }
    fn withdraw(&self, id: u64, first: u64, count: u64) -> Result<u64, Refused> {
        toyos::paged::Pager::withdraw(self, id, first, count)
    }
    fn fail(&self, id: u64, first: u64, count: u64) -> Result<(), Refused> {
        toyos::paged::Pager::fail(self, id, first, count)
    }
    fn resize(&self, id: u64, size: u64, mtime: u64) -> Result<(), Refused> {
        toyos::paged::Pager::resize(self, id, size, mtime)
    }
    fn stamp(&self, id: u64, mtime: u64) -> Result<(), Refused> {
        toyos::paged::Pager::stamp(self, id, mtime)
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
    /// When it was last used, by [`Files::tick`].
    used: u64,
}

impl File {
    fn dirtied(&mut self, first: u64, end: u64) {
        let (lo, hi) = self.dirty.unwrap_or((first, end));
        self.dirty = Some((lo.min(first), hi.max(end)));
    }

    fn droppable(&self) -> bool {
        self.opens == 0 && !self.held && self.dirty.is_none()
    }
}

pub struct Files<P: Pager> {
    pager: Option<P>,
    files: BTreeMap<u64, File>,
    /// Each object's file, by the object's id, which the notices name.
    by_object: BTreeMap<u64, u64>,
    tick: u64,
    /// A supply's or a write-back's pages, kept so one allocates nothing.
    buf: Vec<u8>,
}

impl<P: Pager> Files<P> {
    pub fn new(pager: Option<P>) -> Self {
        Self { pager, files: BTreeMap::new(), by_object: BTreeMap::new(), tick: 0, buf: Vec::new() }
    }

    /// The pager, which reads ready while a notice is owed.
    pub fn pager(&self) -> Option<&P> {
        self.pager.as_ref()
    }

    fn pg(&self) -> &P {
        self.pager.as_ref().expect("fileserver: a paged file on a server with no pager")
    }

    /// The object id of the file `identity` names, for a holder of it.
    pub fn object(&self, identity: u64) -> Option<u64> {
        self.files.get(&identity).map(|f| f.object)
    }

    /// A holder of `identity`'s object went out.
    pub fn held(&mut self, identity: u64) {
        if let Some(f) = self.files.get_mut(&identity) {
            f.held = true;
        }
    }

    /// One line of what is held, for the server's.
    pub fn describe(&self) -> String {
        let dirty = self.files.values().filter(|f| f.dirty.is_some()).count();
        format!("{} paged files, {dirty} dirty", self.files.len())
    }

    fn touch(&mut self, identity: u64) -> Option<&mut File> {
        self.tick += 1;
        let tick = self.tick;
        let f = self.files.get_mut(&identity)?;
        f.used = tick;
        Some(f)
    }

    /// After an open of `identity` at `node` that truncated: its object holds
    /// no page of before. The volume went first, so a refused open changed
    /// nothing, and nothing writes back between the two.
    pub fn truncated(&mut self, volume: &mut dyn Volume, identity: u64, node: Node) -> Result<(), Error> {
        let Some(f) = self.files.get(&identity) else { return Ok(()) };
        let id = f.object;
        let mtime = volume.node_meta(node)?.mtime;
        self.pg().resize(id, 0, mtime).map_err(word)?;
        self.files.get_mut(&identity).expect("found").dirty = None;
        Ok(())
    }

    /// A description opened `identity` at `node`, which the volume holds once
    /// more for it. The object's file holds it once for good, so a second
    /// hold goes back at once.
    pub fn opened(&mut self, volume: &mut dyn Volume, identity: u64, node: Node) -> Result<(), Error> {
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
        // Objects are bounded by count as pages are: a file nobody holds goes
        // to make room for another's.
        let object = loop {
            match self.pg().object(meta.size, meta.mtime) {
                Ok(object) => break object,
                Err(Refused::NoRoom) if self.drop_oldest(volume, identity) => {}
                Err(why) => return Err(word(why)),
            }
        };
        self.files.insert(identity, File { object, node, opens: 1, held: false, dirty: None, used: 0 });
        self.by_object.insert(object, identity);
        self.touch(identity);
        Ok(())
    }

    /// A description of `identity` at `node` closed.
    pub fn closed(&mut self, volume: &mut dyn Volume, identity: u64, node: Node) -> Result<(), Error> {
        match self.files.get_mut(&identity) {
            Some(f) => {
                f.opens = f.opens.saturating_sub(1);
                Ok(())
            }
            None if self.pager.is_none() => volume.close(node),
            None => Ok(()),
        }
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

    /// Up to `out.len()` bytes of `node`'s file at `offset`.
    pub fn read(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, offset: u64, out: Bytes<'_>) -> Result<usize, Error> {
        if self.pager.is_none() || !self.files.contains_key(&identity) {
            return volume.read(node, offset, &mut Window(out));
        }
        let size = volume.node_meta(node)?.size;
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
        let size = volume.node_meta(node)?.size;
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
        match self.oldest(identity, &File::droppable).first() {
            Some(&id) => {
                self.forget(volume, id);
                true
            }
            None => false,
        }
    }

    /// Every dirty file written back, for room: whether any page was.
    fn write_back_some(&mut self, volume: &mut dyn Volume) -> bool {
        let dirty: Vec<u64> = self.files.iter().filter(|(_, f)| f.dirty.is_some()).map(|(id, _)| *id).collect();
        let mut cleaned = false;
        for id in dirty {
            match self.write_back(volume, id) {
                Ok(()) => cleaned = true,
                Err(e) => println!("fileserver: file {id}'s write-back for room failed ({e:?})"),
            }
        }
        cleaned
    }

    /// `data` written to `node`'s file at `offset`.
    pub fn write(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, offset: u64, data: &[u8]) -> Result<(), Error> {
        if self.pager.is_none() || !self.files.contains_key(&identity) {
            return volume.write(node, offset, data);
        }
        let reserved = volume.reserve(node, offset, data.len() as u64)?;
        let old = reserved.size;
        let mtime = match volume.node_meta(node) {
            Ok(meta) => meta.mtime,
            Err(e) => {
                if let Err(again) = volume.unreserve(node, reserved, 0) {
                    println!("fileserver: a write of file {identity} was not cut back to {old} ({again:?}), after {e:?}");
                }
                return Err(e);
            }
        };
        self.touch(identity);
        let id = self.files[&identity].object;
        let mut put = Put { done: 0, held: false };
        let writing = Writing { identity, node, old, contents: reserved.contents(), offset, data, mtime };
        let written = self.put(volume, &writing, &mut put);
        if let Err(e) = &written {
            // What was reserved and not put would read as the blocks' old bytes.
            let landed = if put.done > 0 { offset + put.done as u64 } else { 0 };
            let cut = old.max(landed);
            let resized = self
                .pg()
                .resize(id, cut, mtime)
                .map_err(word)
                .and(volume.unreserve(node, reserved, landed))
                .and_then(|()| self.stamped(volume, identity, node));
            if let Err(again) = resized {
                println!("fileserver: a refused write of file {identity} was not cut back to {cut} ({again:?}), after {e:?}");
            }
        }
        if put.held {
            match self.pg().end_write(id) {
                Ok(()) | Err(Refused::Gone) => {}
                Err(why) => println!("fileserver: file {identity}'s write would not end ({why:?}); its holders ask this server"),
            }
        }
        written
    }

    /// The bytes of a write into the object, each absent page supplied dirty,
    /// counting into `put` what landed and whether a call left the write
    /// under way.
    fn put(&mut self, volume: &mut dyn Volume, w: &Writing<'_>, put: &mut Put) -> Result<(), Error> {
        let Writing { identity, node, old, contents, offset, data, mtime } = *w;
        let end = offset + data.len() as u64;
        let done = &mut put.done;
        while *done < data.len() {
            let at = offset + *done as u64;
            let id = self.files.get(&identity).ok_or(Error::Stale)?.object;
            let wrote = self.pg().write(id, at, &data[*done..], mtime);
            // A first call that takes everything is the whole write; any later
            // call is under the write a short first one left under way.
            put.held |= wrote != Ok(data.len() - *done);
            match wrote {
                Ok(n) => {
                    self.landed(identity, at, n);
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
                // Each page taken is written; one already present was not
                // taken, and the write above puts it on the next turn.
                let taken = self.supplied(volume, identity, page, whole, bytes, true)?;
                self.landed(identity, at, (taken * PAGE) as usize);
                if taken == whole {
                    *done += bytes.len();
                }
                continue;
            }
            // A page in part: what it held below the old end, zeros past the
            // old contents, then the write's.
            let start = page * PAGE;
            let below = old.saturating_sub(start).min(PAGE) as usize;
            let held = contents.saturating_sub(start).min(PAGE) as usize;
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
                self.landed(identity, start, PAGE as usize);
                *done += (upto - at) as usize;
            }
        }
        Ok(())
    }

    /// `len` bytes at `at` landed dirty: the next write-back takes them, a
    /// write-back the same write makes room with included.
    fn landed(&mut self, identity: u64, at: u64, len: usize) {
        if len > 0 {
            let f = self.files.get_mut(&identity).expect("a file being written");
            f.dirtied(at / PAGE, (at + len as u64).div_ceil(PAGE));
        }
    }

    /// `identity`'s volume moved its mtime, which `node` reads: its object
    /// follows.
    pub fn stamped(&mut self, volume: &mut dyn Volume, identity: u64, node: Node) -> Result<(), Error> {
        let Some(f) = self.files.get(&identity) else { return Ok(()) };
        let id = f.object;
        let mtime = volume.node_meta(node)?.mtime;
        self.pg().stamp(id, mtime).map_err(word)
    }

    /// `node`'s file cut or grown to `size`: the volume first, so a refusal
    /// changes nothing, then the object, before anything can write a page
    /// past the size back.
    pub fn truncate(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, size: u64) -> Result<(), Error> {
        volume.truncate(node, size)?;
        if !self.files.contains_key(&identity) {
            return Ok(());
        }
        let mtime = volume.node_meta(node)?.mtime;
        let f = self.files.get_mut(&identity).expect("found");
        if let Some((lo, hi)) = f.dirty {
            let hi = hi.min(size.div_ceil(PAGE));
            f.dirty = (lo < hi).then_some((lo, hi));
        }
        let id = f.object;
        self.pg().resize(id, size, mtime).map_err(word)
    }

    /// `identity`'s dirty pages written to the volume. A page dirtied again
    /// meanwhile, or whose write was refused, stays dirty.
    fn write_back(&mut self, volume: &mut dyn Volume, identity: u64) -> Result<(), Error> {
        let Some(f) = self.files.get(&identity) else { return Ok(()) };
        let Some((lo, hi)) = f.dirty else { return Ok(()) };
        let node = f.node;
        let size = volume.node_meta(node)?.size;
        let hi = hi.min(size.div_ceil(PAGE));
        let mut buf = std::mem::take(&mut self.buf);
        let mut kept: Option<(u64, u64)> = None;
        let mut failed = Ok(());
        let mut first = lo;
        while first < hi {
            let count = (hi - first).min(RANGE);
            let object = self.files[&identity].object;
            let pager = self.pager.as_ref().expect("paged");
            let begun = pager.writeback_begin(object, first, count).map_err(word)?;
            let mut written = Ok(());
            for (start, pages) in runs(&begun, count) {
                let at = (first + start) * PAGE;
                let len = (pages * PAGE).min(size - at) as usize;
                buf.clear();
                buf.resize(len, 0);
                // SAFETY: `buf` is this process's, reached only through these
                // bytes while they live.
                let out = unsafe { Bytes::from_raw(buf.as_mut_ptr(), len) };
                written = match pager.read(object, out, at) {
                    Ok(n) if n == len => volume.write_back(node, at, &buf),
                    Ok(n) => {
                        println!("fileserver: file {identity}'s pages under write-back at {at} read {n} of {len} bytes");
                        Err(Error::Io)
                    }
                    Err(why) => Err(word(why)),
                };
                if written.is_err() {
                    break;
                }
            }
            let still = pager.writeback_end(object, first, count, written.is_ok()).map_err(word)?;
            for (start, pages) in runs(&still, count) {
                let (a, b) = (first + start, first + start + pages);
                let (x, y) = kept.unwrap_or((a, b));
                kept = Some((x.min(a), y.max(b)));
            }
            failed = failed.and(written);
            first += count;
        }
        self.buf = buf;
        self.files.get_mut(&identity).expect("found").dirty = kept;
        failed
    }

    /// Every dirty page to the volume, then the volume synced: the files
    /// whose entries or pages were refused, with why.
    pub fn sync(&mut self, volume: &mut dyn Volume) -> Result<Vec<(Node, Error)>, Error> {
        let dirty: Vec<u64> = self.files.iter().filter(|(_, f)| f.dirty.is_some()).map(|(id, _)| *id).collect();
        let mut refused = Vec::new();
        for id in dirty {
            if let Err(e) = self.write_back(volume, id) {
                refused.push((self.files[&id].node, e));
            }
        }
        let mut unwritten = volume.sync()?;
        unwritten.extend(refused);
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
            .filter(|(id, f)| f.dirty.is_some() && (Some(**id) == named || volume.carried(f.node)))
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

    /// What the pager's objects asked.
    pub fn notice(&mut self, volume: &mut dyn Volume, notice: Notice) {
        match notice {
            Notice::Requested { object, first, count } => {
                let Some(&identity) = self.by_object.get(&object) else { return };
                let count = count.min(RANGE);
                // A reader waits for its pages until they are supplied or
                // failed: a range the volume will not read, or holds none of,
                // is failed.
                let why = match self.fill(volume, identity, first, count) {
                    Ok(0) => "the volume holds none of them".to_string(),
                    Ok(_) => return,
                    Err(e) => format!("{e:?}"),
                };
                println!("fileserver: pages {first}+{count} of file {identity} were not supplied ({why}); failing them");
                if let Err(why) = self.pg().fail(object, first, count) {
                    println!("fileserver: and the failure was refused ({why:?})");
                }
            }
            Notice::NoHolders { object } => {
                if let Some(f) = self.by_object.get(&object).and_then(|identity| self.files.get_mut(identity)) {
                    f.held = false;
                }
            }
        }
    }
}

/// How far a write got: the bytes that landed, and whether a call left the
/// object's write under way.
struct Put {
    done: usize,
    held: bool,
}

/// A write under way: its file, at `offset`, which was `old` bytes long.
#[derive(Clone, Copy)]
struct Writing<'a> {
    identity: u64,
    node: Node,
    old: u64,
    /// The end of the old bytes the volume holds ([`crate::volume::Reserved::contents`]).
    contents: u64,
    offset: u64,
    data: &'a [u8],
    /// The file's mtime from this write, as the volume keeps it.
    mtime: u64,
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
