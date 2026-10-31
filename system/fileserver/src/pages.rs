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
//! **The volume's size is the object's.** A write reserves on the volume
//! first, so the volume's size runs past the object's only for the bytes the
//! write then puts in before it returns; nothing reads the volume for them
//! meanwhile, and a write refused part way is cut back to what it put. A
//! truncation shrinks the object before the volume frees the blocks its
//! dirty pages would have gone to.
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

/// One object's calls, as `toyos::paged::PagedOwner` makes them.
pub trait Object {
    fn read(&self, out: Bytes<'_>, offset: u64) -> Result<usize, Refused>;
    fn supply(&self, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused>;
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<usize, Refused>;
    fn withdraw(&self, first: u64, count: u64) -> Result<u64, Refused>;
    fn fail(&self, first: u64, count: u64) -> Result<(), Refused>;
    fn resize(&self, size: u64) -> Result<(), Refused>;
    fn writeback_begin(&self, first: u64, count: u64) -> Result<[u64; 8], Refused>;
    fn writeback_end(&self, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused>;
}

/// What makes the objects: `toyos::paged::Pager`.
pub trait Pager {
    type Object: Object;
    fn create(&self, size: u64, cookie: u64) -> Result<Self::Object, Refused>;
}

impl Object for toyos::paged::PagedOwner {
    fn read(&self, out: Bytes<'_>, offset: u64) -> Result<usize, Refused> {
        toyos::paged::PagedOwner::read(self, out, offset)
    }
    fn supply(&self, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused> {
        toyos::paged::PagedOwner::supply(self, offset, bytes, dirty)
    }
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<usize, Refused> {
        toyos::paged::PagedOwner::write(self, offset, bytes)
    }
    fn withdraw(&self, first: u64, count: u64) -> Result<u64, Refused> {
        toyos::paged::PagedOwner::withdraw(self, first, count)
    }
    fn fail(&self, first: u64, count: u64) -> Result<(), Refused> {
        toyos::paged::PagedOwner::fail(self, first, count)
    }
    fn resize(&self, size: u64) -> Result<(), Refused> {
        toyos::paged::PagedOwner::resize(self, size)
    }
    fn writeback_begin(&self, first: u64, count: u64) -> Result<[u64; 8], Refused> {
        toyos::paged::PagedOwner::writeback_begin(self, first, count)
    }
    fn writeback_end(&self, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused> {
        toyos::paged::PagedOwner::writeback_end(self, first, count, written)
    }
}

impl Pager for toyos::paged::Pager {
    type Object = toyos::paged::PagedOwner;
    fn create(&self, size: u64, cookie: u64) -> Result<Self::Object, Refused> {
        toyos::paged::PagedOwner::create(self, size, cookie).map_err(Refused::Kernel)
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

struct File<O> {
    object: O,
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

impl<O> File<O> {
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
    files: BTreeMap<u64, File<P::Object>>,
    tick: u64,
    /// A supply's or a write-back's pages, kept so one allocates nothing.
    buf: Vec<u8>,
}

impl<P: Pager> Files<P> {
    pub fn new(pager: Option<P>) -> Self {
        Self { pager, files: BTreeMap::new(), tick: 0, buf: Vec::new() }
    }

    /// The pager, which reads ready while a notice is owed.
    pub fn pager(&self) -> Option<&P> {
        self.pager.as_ref()
    }

    /// The object of the file `identity` names, for a holder of it.
    pub fn object(&self, identity: u64) -> Option<&P::Object> {
        self.files.get(&identity).map(|f| &f.object)
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

    fn touch(&mut self, identity: u64) -> Option<&mut File<P::Object>> {
        self.tick += 1;
        let tick = self.tick;
        let f = self.files.get_mut(&identity)?;
        f.used = tick;
        Some(f)
    }

    /// Before an open of `identity` that truncates: its object holds no page
    /// once the volume frees the blocks.
    pub fn truncating(&mut self, identity: u64) -> Result<(), Error> {
        let Some(f) = self.files.get_mut(&identity) else { return Ok(()) };
        f.object.resize(0).map_err(word)?;
        f.dirty = None;
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
        let size = volume.node_meta(node)?.size;
        let pager = self.pager.as_ref().expect("paged");
        let object = pager.create(size, identity).map_err(word)?;
        self.files.insert(identity, File { object, node, opens: 1, held: false, dirty: None, used: 0 });
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
            drop(f.object);
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
            let f = self.touch(identity).expect("checked");
            match f.object.read(out.sub(done, want - done), at) {
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
            let f = self.files.get(&identity).ok_or(Error::Stale)?;
            match f.object.supply(first * PAGE, bytes, dirty) {
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
        let oldest = |files: &BTreeMap<u64, File<P::Object>>, pick: &dyn Fn(&File<P::Object>) -> bool| {
            let mut by_age: Vec<(u64, u64)> = files.iter().filter(|(id, f)| **id != identity && pick(f)).map(|(id, f)| (f.used, *id)).collect();
            by_age.sort_unstable();
            by_age.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
        };
        if let Some(&id) = oldest(&self.files, &File::droppable).first() {
            self.forget(volume, id);
            return Ok(true);
        }
        for id in oldest(&self.files, &|_| true) {
            if self.files[&id].object.withdraw(0, u64::MAX).map_err(word)? > 0 {
                return Ok(true);
            }
        }
        let own = &self.files[&identity].object;
        if own.withdraw(0, first).map_err(word)? + own.withdraw(first + count, u64::MAX).map_err(word)? > 0 {
            return Ok(true);
        }
        Ok(self.write_back_some(volume))
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
        let old = volume.node_meta(node)?.size;
        volume.reserve(node, offset, data.len() as u64)?;
        self.touch(identity);
        let mut done = 0;
        let put = self.put(volume, &Writing { identity, node, old, offset, data }, &mut done);
        let f = self.files.get_mut(&identity).expect("checked");
        if let Err(e) = put {
            // What was reserved and not put would read as the blocks' old bytes.
            let cut = if done > 0 { old.max(offset + done as u64) } else { old };
            let resized = f.object.resize(cut).map_err(word).and(volume.truncate(node, cut));
            if let Err(again) = resized {
                println!("fileserver: a refused write of file {identity} was not cut back to {cut} ({again:?})");
            }
            return Err(e);
        }
        Ok(())
    }

    /// The bytes of a write into the object, each absent page supplied dirty,
    /// counting into `done` what landed.
    fn put(&mut self, volume: &mut dyn Volume, w: &Writing<'_>, done: &mut usize) -> Result<(), Error> {
        let Writing { identity, node, old, offset, data } = *w;
        let end = offset + data.len() as u64;
        while *done < data.len() {
            let at = offset + *done as u64;
            let f = self.files.get(&identity).ok_or(Error::Stale)?;
            match f.object.write(at, &data[*done..]) {
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
            // A page in part: what it held below the old end, then the write's.
            let start = page * PAGE;
            let below = old.saturating_sub(start).min(PAGE) as usize;
            let upto = end.min(start + PAGE);
            let mut buf = std::mem::take(&mut self.buf);
            buf.clear();
            buf.resize(PAGE as usize, 0);
            let read = volume.read(node, start, &mut Buf(&mut buf[..below]));
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

    /// `node`'s file cut or grown to `size`: the object first, so no dirty
    /// page outlives the blocks it would go to.
    pub fn truncate(&mut self, volume: &mut dyn Volume, identity: u64, node: Node, size: u64) -> Result<(), Error> {
        let Some(f) = self.files.get_mut(&identity) else { return volume.truncate(node, size) };
        let old = volume.node_meta(node)?.size;
        f.object.resize(size).map_err(word)?;
        if let Some((lo, hi)) = f.dirty {
            let hi = hi.min(size.div_ceil(PAGE));
            f.dirty = (lo < hi).then_some((lo, hi));
        }
        volume.truncate(node, size).inspect_err(|_| {
            if size > old {
                let _ = f.object.resize(old);
            }
        })
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
            let object = &self.files[&identity].object;
            let begun = object.writeback_begin(first, count).map_err(word)?;
            let mut written = Ok(());
            for (start, pages) in runs(&begun, count) {
                let at = (first + start) * PAGE;
                let len = (pages * PAGE).min(size - at) as usize;
                buf.clear();
                buf.resize(len, 0);
                // SAFETY: `buf` is this process's, reached only through these
                // bytes while they live.
                let out = unsafe { Bytes::from_raw(buf.as_mut_ptr(), len) };
                written = match object.read(out, at) {
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
            let still = object.writeback_end(first, count, written.is_ok()).map_err(word)?;
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

    /// What the pager's objects asked.
    pub fn notice(&mut self, volume: &mut dyn Volume, notice: Notice) {
        match notice {
            Notice::Requested { cookie, first, count } => {
                if !self.files.contains_key(&cookie) {
                    return;
                }
                let count = count.min(RANGE);
                if let Err(e) = self.fill(volume, cookie, first, count) {
                    // A reader waits for its pages until they are supplied or
                    // failed: one the volume will not read is failed.
                    println!("fileserver: pages {first}+{count} of file {cookie} would not read ({e:?}); failing them");
                    if let Err(why) = self.files[&cookie].object.fail(first, count) {
                        println!("fileserver: and the failure was refused ({why:?})");
                    }
                }
            }
            Notice::NoHolders { cookie } => {
                if let Some(f) = self.files.get_mut(&cookie) {
                    f.held = false;
                }
            }
        }
    }
}

/// A write under way: its file, at `offset`, which was `old` bytes long.
#[derive(Clone, Copy)]
struct Writing<'a> {
    identity: u64,
    node: Node,
    old: u64,
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
