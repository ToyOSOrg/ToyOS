use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use toyos_abi::syscall::SyscallError;

use super::*;
use crate::data::DataVolume;
use crate::ram::Ram;
use crate::volume::OpenHow;
use toyos_blockdev::{Block, BlockDev};

/// The bytes past the first page of the partial write-back's file.
const TAIL: usize = 100;

struct RefusingRead {
    ram: Ram,
    refusing: Rc<Cell<bool>>,
}

impl BlockDev for RefusingRead {
    fn blocks(&self) -> u64 { self.ram.blocks() }
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), toyos_blockdev::Error> {
        self.ram.read(lba, out).await?;
        if self.refusing.get() && out.iter().any(|block| block[..TAIL] == [0xb2; TAIL]) {
            return Err(toyos_blockdev::Error::Device);
        }
        Ok(())
    }
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), toyos_blockdev::Error> {
        self.ram.write(lba, data).await
    }
    async fn flush(&self) -> Result<(), toyos_blockdev::Error> { self.ram.flush().await }
}

impl crate::cache::Disk for RefusingRead {}

/// The kernel's paged objects as far as the server can tell them apart: a
/// table of `capacity` pages, `ceiling` of them dirty or under write-back,
/// and at most `objects` objects.
#[derive(Default)]
struct Model {
    capacity: usize,
    ceiling: usize,
    objects: usize,
    next: u64,
    pages: BTreeMap<(u64, u64), Page>,
    sizes: BTreeMap<u64, u64>,
    anonymous: BTreeSet<u64>,
    supplies: u32,
    /// Every supply from here is refused.
    refusing: bool,
    /// This object's write-backs are refused.
    unwritable: Option<u64>,
    /// Objects whose writes the pager holds.
    holding: BTreeSet<u64>,
    /// A holder's write to try while the pager's next write call is under
    /// way, and what it was answered.
    during_write: Option<(u64, u64, Vec<u8>)>,
    during_answer: Option<Option<usize>>,
    /// Pages of credit granted, all told.
    granted: u64,
    /// Whether the last time stamped was stamped under the pager's hold.
    last_stamp_held: Option<bool>,
    /// Each object's pages as a holder could read them after each change
    /// that left no write under way.
    views: Vec<(u64, BTreeMap<u64, Vec<u8>>)>,
    /// Each object's credit, time and record, and the pages holders dirtied
    /// since the pager last took them.
    credit: BTreeMap<u64, u64>,
    mtimes: BTreeMap<u64, u64>,
    records: BTreeMap<u64, [u8; PAGED_RECORD]>,
    dirtied: BTreeMap<u64, (u64, u64)>,
}

struct Page {
    bytes: Vec<u8>,
    dirty: bool,
    writeback: bool,
    /// A holder's credit paid for its dirtying (`kernel::paged`'s rule).
    credited: bool,
    credit_in_flight: bool,
}

impl Page {
    fn new(bytes: Vec<u8>, dirty: bool) -> Self {
        Self { bytes, dirty, writeback: false, credited: false, credit_in_flight: false }
    }
}

impl Model {
    fn held(&self) -> usize {
        self.pages.values().filter(|p| p.dirty || p.writeback).count()
    }

    /// A change to `id` ended: what a holder reads of it, unless the pager
    /// holds its writes, whose unhold shows them whole.
    fn changed(&mut self, id: u64) {
        if !self.holding.contains(&id) {
            let view = self.pages.iter().filter(|((c, _), _)| *c == id).map(|(&(_, p), page)| (p, page.bytes.clone())).collect();
            self.views.push((id, view));
        }
    }

    /// The kernel refuses the pager's writes and truncations without its hold.
    fn must_hold(&self, id: u64) {
        assert!(self.holding.contains(&id), "a write of object {id} without the pager's hold");
    }

    /// A page leaves before its write-back: the room its credit paid for is
    /// credit again.
    fn removed(&mut self, id: u64, page: Page) {
        if page.credited {
            *self.credit.entry(id).or_default() += 1;
        }
    }
}

#[derive(Clone)]
struct Fake(Rc<RefCell<Model>>);

impl Fake {
    fn new(capacity: usize, ceiling: usize) -> Self {
        Self(Rc::new(RefCell::new(Model { capacity, ceiling, objects: usize::MAX, ..Model::default() })))
    }

    fn pages_of(&self, object: u64) -> usize {
        self.0.borrow().pages.keys().filter(|(c, _)| *c == object).count()
    }

    fn supply_pages(&self, id: u64, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused> {
        let mut m = self.0.borrow_mut();
        let anonymous = m.anonymous.contains(&id);
        m.supplies += 1;
        if m.refusing {
            return Err(Refused::Kernel(SyscallError::Io));
        }
        let mut installed = 0;
        for (k, chunk) in bytes.chunks(PAGE as usize).enumerate() {
            let page = offset / PAGE + k as u64;
            let size = m.sizes[&id];
            let why = if !dirty && page >= size.div_ceil(PAGE) {
                Some(Refused::Kernel(SyscallError::InvalidArgument))
            } else if m.pages.contains_key(&(id, page)) {
                if dirty { return Ok(installed); }
                continue;
            } else if m.pages.len() >= m.capacity {
                Some(Refused::NoRoom)
            } else if dirty && !anonymous && m.held() >= m.ceiling {
                Some(Refused::DirtyFull)
            } else {
                None
            };
            if let Some(why) = why {
                return if installed > 0 { Ok(installed) } else { Err(why) };
            }
            let mut held = vec![0u8; PAGE as usize];
            let n = if dirty { chunk.len() } else { chunk.len().min((size - page * PAGE) as usize) };
            held[..n].copy_from_slice(&chunk[..n]);
            m.pages.insert((id, page), Page::new(held, dirty && !anonymous));
            if dirty {
                let s = m.sizes.get_mut(&id).unwrap();
                *s = (*s).max(page * PAGE + chunk.len() as u64);
            }
            installed += 1;
        }
        Ok(installed)
    }

    fn write_pages(&self, id: u64, offset: u64, bytes: &[u8]) -> Result<usize, Refused> {
        let mut m = self.0.borrow_mut();
        let anonymous = m.anonymous.contains(&id);
        let mut done = 0;
        while done < bytes.len() {
            let at = offset + done as u64;
            let key = (id, at / PAGE);
            let Some(page) = m.pages.get(&key) else { break };
            if !anonymous && !page.dirty && !page.writeback && m.held() >= m.ceiling {
                return if done > 0 { Ok(done) } else { Err(Refused::DirtyFull) };
            }
            let within = (at % PAGE) as usize;
            let n = (PAGE as usize - within).min(bytes.len() - done);
            let page = m.pages.get_mut(&key).unwrap();
            page.bytes[within..within + n].copy_from_slice(&bytes[done..done + n]);
            page.dirty = !anonymous;
            // The pager's write brings its own room.
            let refund = std::mem::take(&mut page.credited);
            if refund {
                *m.credit.entry(id).or_default() += 1;
            }
            let s = m.sizes.get_mut(&id).unwrap();
            *s = (*s).max(at + n as u64);
            done += n;
        }
        if done == 0 { Err(Refused::Absent) } else { Ok(done) }
    }

    /// A holder's write as the kernel admits it
    /// (`kernel::paged::Pages::holder_fits`): whole at `mtime`, or `None`
    /// with nothing written where a page inside the size is absent, or the
    /// credit or the dirty ceiling is short.
    fn holder_write(&self, id: u64, offset: u64, bytes: &[u8], mtime: u64) -> Option<usize> {
        let mut m = self.0.borrow_mut();
        if m.holding.contains(&id) {
            return None;
        }
        let anonymous = m.anonymous.contains(&id);
        let (first, end) = (offset / PAGE, (offset + bytes.len() as u64).div_ceil(PAGE));
        let reach = m.sizes[&id].div_ceil(PAGE);
        let (mut dirtying, mut held) = (0u64, 0usize);
        for p in first..end {
            match m.pages.get(&(id, p)) {
                Some(page) => {
                    dirtying += u64::from(!page.dirty);
                    held += usize::from(!page.dirty && !page.writeback);
                }
                None if p >= reach || anonymous => {
                    dirtying += 1;
                    held += 1;
                }
                None => return None,
            }
        }
        if !anonymous && (dirtying > m.credit.get(&id).copied().unwrap_or(0) || m.held() + held > m.ceiling) {
            return None;
        }
        for p in first..end {
            let page = m.pages.entry((id, p)).or_insert_with(|| Page::new(vec![0; PAGE as usize], false));
            let (from, to) = ((p * PAGE).max(offset), ((p + 1) * PAGE).min(offset + bytes.len() as u64));
            page.bytes[(from - p * PAGE) as usize..(to - p * PAGE) as usize]
                .copy_from_slice(&bytes[(from - offset) as usize..(to - offset) as usize]);
            if !anonymous && !page.dirty {
                page.dirty = true;
                page.credited = true;
                *m.credit.get_mut(&id).unwrap() -= 1;
                let (lo, hi) = m.dirtied.get(&id).copied().unwrap_or((p, p + 1));
                m.dirtied.insert(id, (lo.min(p), hi.max(p + 1)));
            }
        }
        let size = m.sizes.get_mut(&id).unwrap();
        *size = (*size).max(offset + bytes.len() as u64);
        m.mtimes.insert(id, mtime);
        m.changed(id);
        Some(bytes.len())
    }

    /// The holder's write [`Model::during_write`] names, tried once, while a
    /// write of the pager's is under way.
    fn during_write(&self) {
        let pending = self.0.borrow_mut().during_write.take();
        if let Some((id, offset, bytes)) = pending {
            let answer = self.holder_write(id, offset, &bytes, 9);
            self.0.borrow_mut().during_answer = Some(answer);
        }
    }
}

impl Pager for Fake {
    fn grant(&self, id: u64, pages: u64) -> Result<u64, Refused> {
        let mut m = self.0.borrow_mut();
        m.granted += pages;
        let credit = m.credit.entry(id).or_default();
        *credit += pages;
        Ok(*credit)
    }

    fn take_back(&self, id: u64) -> Result<u64, Refused> {
        Ok(self.0.borrow_mut().credit.remove(&id).unwrap_or(0))
    }

    fn set_record(&self, id: u64, record: Option<&[u8; PAGED_RECORD]>, mtime: Option<u64>) -> Result<(), Refused> {
        let mut m = self.0.borrow_mut();
        if let Some(record) = record {
            m.records.insert(id, *record);
        }
        if let Some(mtime) = mtime {
            m.mtimes.insert(id, mtime);
            m.last_stamp_held = Some(m.holding.contains(&id));
        }
        Ok(())
    }

    fn status(&self, id: u64) -> Result<PagedStatus, Refused> {
        let m = self.0.borrow();
        let size = *m.sizes.get(&id).ok_or(Refused::Gone)?;
        Ok(PagedStatus { size, mtime: m.mtimes.get(&id).copied().unwrap_or(0), credit: m.credit.get(&id).copied().unwrap_or(0) })
    }

    fn take_dirtied(&self, id: u64) -> Result<Option<(u64, u64)>, Refused> {
        Ok(self.0.borrow_mut().dirtied.remove(&id).map(|(lo, hi)| (lo, hi - lo)))
    }

    fn room(&self) -> Result<u64, Refused> {
        let model = self.0.borrow();
        Ok((model.capacity - model.pages.len()) as u64)
    }

    fn object(&self, size: u64) -> Result<u64, Refused> {
        let mut m = self.0.borrow_mut();
        if m.sizes.len() >= m.objects {
            return Err(Refused::NoRoom);
        }
        m.next += 1;
        let id = m.next;
        m.sizes.insert(id, size);
        Ok(id)
    }

    fn anonymous(&self, size: u64) -> Result<u64, Refused> {
        let id = self.object(size)?;
        self.0.borrow_mut().anonymous.insert(id);
        Ok(id)
    }

    fn release(&self, id: u64) -> Result<(), Refused> {
        let mut m = self.0.borrow_mut();
        m.pages.retain(|(c, _), _| *c != id);
        m.sizes.remove(&id).map(drop).ok_or(Refused::Gone)
    }

    fn read(&self, id: u64, out: Bytes<'_>, offset: u64) -> Result<usize, Refused> {
        let m = self.0.borrow();
        let size = m.sizes[&id];
        let end = size.min(offset + out.len() as u64);
        let mut at = offset;
        while at < end {
            let within = (at % PAGE) as usize;
            let n = ((PAGE as usize - within) as u64).min(end - at) as usize;
            match m.pages.get(&(id, at / PAGE)) {
                Some(page) => out.write((at - offset) as usize, &page.bytes[within..within + n]),
                None if m.anonymous.contains(&id) => out.sub((at - offset) as usize, n).zero(),
                None => break,
            }
            at += n as u64;
        }
        match at - offset {
            0 if offset < end => Err(Refused::Absent),
            n => Ok(n as usize),
        }
    }

    fn supply(&self, id: u64, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused> {
        if dirty {
            self.0.borrow().must_hold(id);
            self.during_write();
        }
        self.supply_pages(id, offset, bytes, dirty)
    }

    fn write(&self, id: u64, offset: u64, bytes: &[u8]) -> Result<usize, Refused> {
        self.0.borrow().must_hold(id);
        self.during_write();
        self.write_pages(id, offset, bytes)
    }

    fn hold(&self, id: u64) -> Result<u64, Refused> {
        let mut m = self.0.borrow_mut();
        let size = *m.sizes.get(&id).ok_or(Refused::Gone)?;
        assert!(m.holding.insert(id), "object {id} held twice");
        Ok(size)
    }

    fn unhold(&self, id: u64) -> Result<(), Refused> {
        let mut m = self.0.borrow_mut();
        assert!(m.holding.remove(&id), "object {id} given back unheld");
        m.changed(id);
        Ok(())
    }

    fn withdraw(&self, id: u64, first: u64, count: u64) -> Result<u64, Refused> {
        let mut m = self.0.borrow_mut();
        if m.anonymous.contains(&id) { return Ok(0); }
        let before = m.pages.len();
        let end = first.saturating_add(count);
        m.pages.retain(|(c, p), page| *c != id || !(first..end).contains(p) || page.dirty || page.writeback);
        Ok((before - m.pages.len()) as u64)
    }

    fn fail(&self, id: u64, first: u64, count: u64) -> Result<(), Refused> {
        let mut m = self.0.borrow_mut();
        let gone: Vec<u64> = m.pages.keys().filter(|(c, p)| *c == id && (first..first + count).contains(p)).map(|&(_, p)| p).collect();
        for p in gone {
            let page = m.pages.remove(&(id, p)).unwrap();
            m.removed(id, page);
        }
        Ok(())
    }

    fn resize(&self, id: u64, size: u64) -> Result<(), Refused> {
        let mut m = self.0.borrow_mut();
        m.must_hold(id);
        let gone: Vec<u64> = m.pages.keys().filter(|(c, p)| *c == id && *p >= size.div_ceil(PAGE)).map(|&(_, p)| p).collect();
        for p in gone {
            let page = m.pages.remove(&(id, p)).unwrap();
            m.removed(id, page);
        }
        if let Some(page) = m.pages.get_mut(&(id, size / PAGE)) {
            page.bytes[(size % PAGE) as usize..].fill(0);
        }
        m.sizes.insert(id, size);
        Ok(())
    }

    fn writeback_begin(&self, id: u64, first: u64, count: u64) -> Result<[u64; 8], Refused> {
        if self.0.borrow().unwritable == Some(id) {
            return Err(Refused::Kernel(SyscallError::Io));
        }
        let mut bits = [0u64; 8];
        for (&(c, p), page) in self.0.borrow_mut().pages.iter_mut() {
            if c == id && (first..first + count).contains(&p) && page.dirty {
                page.dirty = false;
                page.writeback = true;
                page.credit_in_flight = std::mem::take(&mut page.credited);
                bits[((p - first) / 64) as usize] |= 1 << ((p - first) % 64);
            }
        }
        Ok(bits)
    }

    fn writeback_end(&self, id: u64, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused> {
        let mut bits = [0u64; 8];
        for (&(c, p), page) in self.0.borrow_mut().pages.iter_mut() {
            if c == id && (first..first + count).contains(&p) && page.writeback {
                page.writeback = false;
                page.dirty |= !written;
                page.credited |= std::mem::take(&mut page.credit_in_flight) && !written;
                if page.dirty {
                    bits[((p - first) / 64) as usize] |= 1 << ((p - first) % 64);
                }
            }
        }
        Ok(bits)
    }
}

fn clock() -> u64 {
    1
}

fn volume() -> DataVolume<Ram> {
    DataVolume::format(Ram::new(16384), 256, &["home"], clock).unwrap()
}

const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };

const RECORD: Record = Record { kind: toyos::fs::KIND_FILE, nlink: 1, atime: 0, ctime: 0, volume: 1, node: 1 };

/// `path` opened through `files`: its node and identity.
fn open(files: &mut Files<Fake>, v: &mut dyn Volume, path: &str) -> (Node, u64) {
    let node = v.open(path, CREATE).unwrap();
    let identity = v.identity(path).unwrap();
    files.opened(v, identity, node, RECORD).unwrap();
    (node, identity)
}

fn read(files: &mut Files<Fake>, v: &mut dyn Volume, (node, id): (Node, u64), offset: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0xEEu8; len];
    // SAFETY: `out` is the test's, reached only through these bytes meanwhile.
    let bytes = unsafe { Bytes::from_raw(out.as_mut_ptr(), len) };
    let n = files.read(v, id, node, offset, bytes).unwrap();
    out.truncate(n);
    out
}

/// What the volume itself holds of `path`, past every object.
fn on_volume(v: &mut DataVolume<Ram>, path: &str) -> Vec<u8> {
    let node = v.open(path, CREATE).unwrap();
    let mut out = vec![0u8; v.node_meta(node).unwrap().size as usize];
    let n = v.read(node, 0, &mut Buf(&mut out)).unwrap();
    v.close(node).unwrap();
    out.truncate(n);
    out
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

#[test]
fn memory_bytes_survive_close_sync_and_namespace_replacement_until_the_last_holder() {
    let mut volume = crate::memory::MemoryVolume::new(&[], clock);
    let fake = Fake::new(4, 0);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut volume, "file");
    files.write(&mut volume, id, node, Some(0), b"old contents").unwrap();
    files.closed(&mut volume, id, node).unwrap();
    files.sync(&mut volume).unwrap();
    let again = open(&mut files, &mut volume, "file");
    assert_eq!(again, (node, id));
    assert_eq!(read(&mut files, &mut volume, again, 0, 20), b"old contents");
    let replacement = open(&mut files, &mut volume, "replacement");
    files.write(&mut volume, replacement.1, replacement.0, Some(0), b"new contents").unwrap();
    volume.rename("replacement", "file").unwrap();
    files.unlinked(&mut volume, id).unwrap();
    assert_eq!(read(&mut files, &mut volume, again, 0, 20), b"old contents");
    assert_eq!(volume.identity("file"), Ok(replacement.1));
    let object = files.object(id).unwrap();
    files.held(&mut volume, id, false);
    files.closed(&mut volume, id, node).unwrap();
    assert!(files.object(id).is_some(), "the paged holder outlives the description");
    files.notice(&mut volume, Notice::NoHolders { object });
    assert!(files.object(id).is_none());
    assert_eq!(volume.node_meta(node), Err(Error::Stale));
    assert_eq!(read(&mut files, &mut volume, replacement, 0, 20), b"new contents");
    assert_eq!(fake.0.borrow().held(), 0, "anonymous bytes need no writeback slots");
}

#[test]
fn persistent_unlink_preserves_dirty_bytes_until_the_last_holder() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake));
    let (node, id) = open(&mut files, &mut v, "home/held");
    files.write(&mut v, id, node, Some(0), b"still held").unwrap();
    files.held(&mut v, id, false);
    let object = files.object(id).unwrap();
    v.unlink("home/held").unwrap();
    files.unlinked(&mut v, id).unwrap();
    assert_eq!(read(&mut files, &mut v, (node, id), 0, 10), b"still held");
    files.closed(&mut v, id, node).unwrap();
    assert_eq!(files.object(id), Some(object));
    files.notice(&mut v, Notice::NoHolders { object });
    assert_eq!(files.object(id), None);
    assert_eq!(v.node_meta(node), Err(Error::Stale));
    assert!(v.sync().unwrap().is_empty());
}

#[test]
fn memory_sparse_files_do_not_resurrect_truncated_bytes_or_evict_existing_data() {
    let mut volume = crate::memory::MemoryVolume::new(&[], clock);
    let fake = Fake::new(2, 0);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut volume, "file");
    files.write(&mut volume, id, node, Some(PAGE + 5), b"tail").unwrap();
    assert_eq!(fake.pages_of(files.object(id).unwrap()), 1, "the hole holds no pages");
    assert_eq!(files.room(&mut volume).unwrap().free, PAGE);
    let bytes = read(&mut files, &mut volume, (node, id), 0, 2 * PAGE as usize);
    assert_eq!(files.room(&mut volume).unwrap().free, PAGE, "reading a hole retains no zero page");
    assert!(bytes[..PAGE as usize + 5].iter().all(|&b| b == 0));
    assert_eq!(&bytes[PAGE as usize + 5..], b"tail");
    files.truncate(&mut volume, id, node, PAGE + 6).unwrap();
    files.truncate(&mut volume, id, node, PAGE + 9).unwrap();
    assert_eq!(read(&mut files, &mut volume, (node, id), PAGE + 5, 4), b"t\0\0\0");
    files.write(&mut volume, id, node, Some(0), b"head").unwrap();
    let (other, other_id) = open(&mut files, &mut volume, "other");
    assert_eq!(files.write(&mut volume, other_id, other, Some(0), b"full"), Err(Error::NoSpace));
    assert_eq!(volume.node_meta(other).unwrap().size, 0);
    assert_eq!(read(&mut files, &mut volume, (node, id), PAGE + 5, 4), b"t\0\0\0");
    files.truncate(&mut volume, id, node, PAGE).unwrap();
    files.write(&mut volume, other_id, other, Some(0), b"room").unwrap();
    assert_eq!(read(&mut files, &mut volume, (other, other_id), 0, 4), b"room");
}

#[test]
fn a_read_is_supplied_off_the_volume_once_and_then_served_from_the_object() {
    let mut v = volume();
    let data = pattern(3 * PAGE as usize + 100, 1);
    let n = v.open("home/f", CREATE).unwrap();
    v.write(n, 0, &data).unwrap();
    v.close(n).unwrap();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let at = open(&mut files, &mut v, "home/f");
    assert_eq!(read(&mut files, &mut v, at, 0, data.len() + 50), data, "the whole file, and no byte past it");
    assert_eq!(fake.0.borrow().supplies, 1, "four pages, one supply");
    assert_eq!(read(&mut files, &mut v, at, 5000, 300), &data[5000..5300]);
    assert_eq!(fake.0.borrow().supplies, 1, "a second read asks the volume nothing");
}

#[test]
fn a_write_lands_in_the_object_and_reaches_the_volume_at_the_sync() {
    let mut v = volume();
    let old = pattern(6000, 2);
    let n = v.open("home/f", CREATE).unwrap();
    v.write(n, 0, &old).unwrap();
    v.close(n).unwrap();
    let mut files = Files::new(Some(Fake::new(64, 16)));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, Some(5000), b"new bytes over the end of the file").unwrap();
    files.write(&mut v, id, node, Some(5990), b"and past it").unwrap();
    let mut want = old.clone();
    want[5000..5034].copy_from_slice(b"new bytes over the end of the file");
    want.truncate(5990);
    want.extend_from_slice(b"and past it");
    let got = read(&mut files, &mut v, (node, id), 0, 8000);
    let first = got.iter().zip(&want).position(|(a, b)| a != b);
    assert_eq!((got.len(), first), (want.len(), None), "the old bytes below, the write's above");
    assert_eq!(on_volume(&mut v, "home/f")[..4096], old[..4096], "nothing written back yet");
    assert_eq!(files.sync(&mut v).unwrap(), []);
    assert_eq!(on_volume(&mut v, "home/f"), want);
}

#[test]
fn a_write_past_the_dirty_ceiling_writes_back_and_goes_on() {
    let mut v = volume();
    let mut files = Files::new(Some(Fake::new(64, 4)));
    let (node, id) = open(&mut files, &mut v, "home/f");
    let data = pattern(16 * PAGE as usize, 3);
    files.write(&mut v, id, node, Some(0), &data).unwrap();
    assert_eq!(on_volume(&mut v, "home/f")[..PAGE as usize], data[..PAGE as usize], "written back to make room");
    files.sync(&mut v).unwrap();
    assert_eq!(on_volume(&mut v, "home/f"), data);
}

#[test]
fn room_is_made_by_a_file_nobody_holds_before_any_other() {
    let mut v = volume();
    for path in ["home/a", "home/b", "home/c"] {
        let n = v.open(path, CREATE).unwrap();
        v.write(n, 0, &pattern(4 * PAGE as usize, 4)).unwrap();
        v.close(n).unwrap();
    }
    let fake = Fake::new(8, 4);
    let mut files = Files::new(Some(fake.clone()));
    let a = open(&mut files, &mut v, "home/a");
    read(&mut files, &mut v, a, 0, 4 * PAGE as usize);
    let b = open(&mut files, &mut v, "home/b");
    read(&mut files, &mut v, b, 0, 4 * PAGE as usize);
    files.closed(&mut v, b.1, b.0).unwrap();
    let c = open(&mut files, &mut v, "home/c");
    assert_eq!(read(&mut files, &mut v, c, 0, 4 * PAGE as usize), pattern(4 * PAGE as usize, 4));
    assert!(files.object(b.1).is_none(), "the closed file's object went");
    assert_eq!(fake.pages_of(files.object(a.1).unwrap()), 4, "the open file, older, kept its pages");
    assert_eq!(fake.pages_of(files.object(c.1).unwrap()), 4);
}

#[test]
fn a_shrink_drops_the_dirty_pages_before_the_volume_frees_their_blocks() {
    let mut v = volume();
    let mut files = Files::new(Some(Fake::new(64, 16)));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, Some(0), &pattern(3 * PAGE as usize, 5)).unwrap();
    files.truncate(&mut v, id, node, 1000).unwrap();
    files.truncate(&mut v, id, node, 3 * PAGE).unwrap();
    files.sync(&mut v).unwrap();
    let held = on_volume(&mut v, "home/f");
    assert_eq!(held.len(), 3 * PAGE as usize);
    assert!(held[1000..].iter().all(|&b| b == 0), "no dirty page outlived the shrink");
    assert_eq!(held[..1000], pattern(1000, 5)[..]);
}

#[test]
fn a_write_refused_part_way_is_cut_back_to_what_it_put() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, Some(0), b"kept").unwrap();
    fake.0.borrow_mut().refusing = true;
    assert!(files.write(&mut v, id, node, Some(PAGE), &pattern(3 * PAGE as usize, 6)).is_err());
    assert_eq!(v.node_meta(node).unwrap().size, 4, "the volume's size is back where the object's is");
    fake.0.borrow_mut().refusing = false;
    files.sync(&mut v).unwrap();
    assert_eq!(on_volume(&mut v, "home/f"), b"kept");
}

#[test]
fn an_overwrite_keeps_incoming_capacity_while_its_older_dirty_page_is_flushed() {
    let mut v = volume();
    let fake = Fake::new(4, 1);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, Some(0), &pattern(2 * PAGE as usize, 1)).unwrap();
    files.fsync(&mut v, Some((id, node))).unwrap();
    files.write(&mut v, id, node, Some(PAGE), &pattern(PAGE as usize, 2)).unwrap();
    let object = files.object(id).unwrap();
    assert!(!fake.0.borrow().pages[&(object, 0)].dirty);
    assert!(fake.0.borrow().pages[&(object, 1)].dirty);

    let replacement = pattern(2 * PAGE as usize, 3);
    files.write(&mut v, id, node, Some(0), &replacement).unwrap();
    files.fsync(&mut v, Some((id, node))).unwrap();
    let mut v = after_a_stop(v);
    assert_eq!(on_volume(&mut v, "home/f"), replacement);
}

#[test]
/// Two pages each under its own write's capacity, the tail written first so
/// the second write does not continue it, written back in one run: the
/// second page, the file's last in part, reads the old block it keeps the
/// rest of, and that read is refused.
fn a_partial_writeback_cleans_only_its_prefix_and_retries_the_same_reserved_suffix() {
    let refusing = Rc::new(Cell::new(false));
    let disk = RefusingRead { ram: Ram::new(16384), refusing: Rc::clone(&refusing) };
    let mut v = DataVolume::format(disk, 256, &["home"], clock).unwrap();
    let node = v.open("home/f", CREATE).unwrap();
    let mut old = vec![0xa1; PAGE as usize];
    old.extend([0xb2; TAIL]);
    v.write(node, 0, &old).unwrap();
    v.close(node).unwrap();
    v.sync().unwrap();

    let fake = Fake::new(4, 4);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    let new = pattern(PAGE as usize + TAIL, 8);
    files.write(&mut v, id, node, Some(PAGE), &new[PAGE as usize..]).unwrap();
    files.write(&mut v, id, node, Some(0), &new[..PAGE as usize]).unwrap();
    refusing.set(true);
    assert_eq!(files.fsync(&mut v, Some((id, node))), Err(Error::Io));
    let object = files.object(id).unwrap();
    let m = fake.0.borrow();
    assert!(!m.pages[&(object, 0)].dirty, "the successful prefix must not consume its reservation twice");
    assert!(m.pages[&(object, 1)].dirty, "the refused suffix still owes its bytes");
    drop(m);
    refusing.set(false);
    files.fsync(&mut v, Some((id, node))).unwrap();
    let mut v = match DataVolume::probe(v.stopped(), 256, &["home"], clock) {
        crate::data::Probed::Mounted(v) => *v,
        _ => panic!("the committed file must mount"),
    };
    let node = v.open("home/f", CREATE).unwrap();
    let mut bytes = vec![0; new.len()];
    assert_eq!(v.read(node, 0, &mut Buf(&mut bytes)), Ok(bytes.len()));
    assert_eq!(bytes, new);
}

#[test]
fn cancelling_an_overwrite_before_copy_keeps_the_older_dirty_reservation() {
    let mut v = volume();
    let fake = Fake::new(4, 4);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    let old = pattern(PAGE as usize, 4);
    files.write(&mut v, id, node, Some(PAGE), &old).unwrap();
    fake.0.borrow_mut().refusing = true;
    assert_eq!(files.write(&mut v, id, node, Some(0), &pattern(2 * PAGE as usize, 5)), Err(Error::Io));
    fake.0.borrow_mut().refusing = false;
    files.fsync(&mut v, Some((id, node))).unwrap();
    let mut v = after_a_stop(v);
    let bytes = on_volume(&mut v, "home/f");
    assert!(bytes[..PAGE as usize].iter().all(|&b| b == 0));
    assert_eq!(bytes[PAGE as usize..], old);
}

#[test]
fn a_supply_stops_before_a_present_page_and_resumes_after_the_write_copies_it() {
    let mut v = volume();
    let fake = Fake::new(4, 4);
    let mut files = Files::new(Some(fake));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, Some(PAGE), &pattern(PAGE as usize, 6)).unwrap();
    let replacement = pattern(3 * PAGE as usize, 7);
    files.write(&mut v, id, node, Some(0), &replacement).unwrap();
    files.fsync(&mut v, Some((id, node))).unwrap();
    let mut v = after_a_stop(v);
    assert_eq!(on_volume(&mut v, "home/f"), replacement);
}

/// A write into a hole the file was extended over, refused before any byte
/// landed: the block its reserve took there goes back, though the hole lies
/// inside the old size, and the hole still reads zeros.
#[test]
fn a_write_refused_in_a_hole_gives_back_the_block_it_took() {
    // Every block the volume does not write holds what an earlier tenant left.
    let disk = Ram::new(16384);
    let stale = vec![[0xAB; toyos_blockdev::BLOCK]; 16384];
    toyos_blockdev::block_on(toyos_blockdev::BlockDev::write(&disk, 0, &stale), || ()).unwrap();
    let mut v = DataVolume::format(disk, 256, &["home"], clock).unwrap();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, Some(0), &pattern(PAGE as usize, 4)).unwrap();
    files.truncate(&mut v, id, node, 3 * PAGE).unwrap();
    files.sync(&mut v).unwrap();
    let room = v.room().unwrap().free;
    fake.0.borrow_mut().refusing = true;
    for _ in 0..2 {
        assert_eq!(files.write(&mut v, id, node, Some(PAGE), &pattern(PAGE as usize, 5)), Err(Error::Io));
        assert_eq!(files.sync(&mut v).unwrap(), [], "a cancelled write left capacity awaiting bytes");
    }
    // Reservation admission may compact older metadata, increasing free
    // space. It must not keep capacity for either refused write.
    assert!(v.room().unwrap().free >= room, "the refused writes kept capacity");
    assert_eq!(v.node_meta(node).unwrap().size, 3 * PAGE);
    fake.0.borrow_mut().refusing = false;
    let mut out = vec![0xEEu8; PAGE as usize];
    assert_eq!(v.read(node, PAGE, &mut Buf(&mut out)).unwrap(), PAGE as usize);
    assert!(out.iter().all(|&b| b == 0), "the hole reads {:#04x}, not zeros", out[out.iter().position(|&b| b != 0).unwrap()]);
}

/// A byte written into a hole below the old end, as the server writes one:
/// a holder and the volume after write-back read zeros around it, not what
/// the block its reserve took last held.
#[test]
fn a_write_in_part_into_a_hole_supplies_zeros_around_it() {
    let disk = Ram::new(16384);
    let stale = vec![[0xAB; toyos_blockdev::BLOCK]; 16384];
    toyos_blockdev::block_on(toyos_blockdev::BlockDev::write(&disk, 0, &stale), || ()).unwrap();
    let mut v = DataVolume::format(disk, 256, &["home"], clock).unwrap();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, Some(0), &pattern(PAGE as usize, 4)).unwrap();
    files.truncate(&mut v, id, node, 3 * PAGE).unwrap();
    files.sync(&mut v).unwrap();
    files.write(&mut v, id, node, Some(6000), &[2]).unwrap();

    let mut want = vec![0u8; PAGE as usize];
    want[6000 - PAGE as usize] = 2;
    assert!(read(&mut files, &mut v, (node, id), PAGE, PAGE as usize) == want, "a holder reads another file's bytes");
    files.sync(&mut v).unwrap();
    let mut out = vec![0xEEu8; PAGE as usize];
    assert_eq!(v.read(node, PAGE, &mut Buf(&mut out)).unwrap(), PAGE as usize);
    assert!(out == want, "the volume holds another file's bytes after write-back");
}

#[test]
fn a_requested_page_is_supplied_by_its_notice() {
    let mut v = volume();
    let n = v.open("home/f", CREATE).unwrap();
    v.write(n, 0, &pattern(2 * PAGE as usize, 7)).unwrap();
    v.close(n).unwrap();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (_, id) = open(&mut files, &mut v, "home/f");
    let object = files.object(id).unwrap();
    files.notice(&mut v, Notice::Requested { object, first: 1, count: 1 });
    assert_eq!(fake.pages_of(files.object(id).unwrap()), 1, "the page asked for, and no other");
    files.notice(&mut v, Notice::Requested { object, first: 9, count: 4 });
    assert_eq!(fake.pages_of(files.object(id).unwrap()), 1, "nothing past the size");
}

/// An open that truncates, as the server makes one: the object is emptied
/// first, so neither a read nor a later write-back finds a page of before.
#[test]
fn a_truncating_open_leaves_the_object_no_page_of_before() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, Some(0), &pattern(2 * PAGE as usize, 8)).unwrap();
    
    let truncate = OpenHow { create: false, create_new: false, truncate: true };
    let again = v.open("home/f", truncate).unwrap();
    files.truncated(&mut v, id, again).unwrap();
    files.opened(&mut v, id, again, RECORD).unwrap();
    let object = files.object(id).unwrap();
    assert_eq!((fake.0.borrow().sizes[&object], fake.pages_of(object)), (0, 0), "a holder reads the object, and finds nothing of before");
    assert_eq!(read(&mut files, &mut v, (again, id), 0, 100), b"", "no byte of before");
    assert_eq!(files.sync(&mut v).unwrap(), []);
    assert_eq!(on_volume(&mut v, "home/f"), b"");
}

/// `osbench`'s `fs.disk.seq_read.4096` at small scale: a file written and
/// synced, then opened, read whole in pages and closed, in rounds, under a
/// pager that holds twice the file. A round after the first asks the volume
/// nothing: a close keeps the file's object, and its pages with it.
#[test]
fn a_file_the_pager_holds_is_read_again_from_its_object_across_closes() {
    const PAGES: usize = 64;
    let mut v = volume();
    let n = v.open("home/seq", CREATE).unwrap();
    v.write(n, 0, &pattern(PAGES * PAGE as usize, 9)).unwrap();
    v.close(n).unwrap();
    let fake = Fake::new(2 * PAGES, 16);
    let mut files = Files::new(Some(fake.clone()));
    let round = |files: &mut Files<Fake>, v: &mut DataVolume<Ram>| {
        let before = fake.0.borrow().supplies;
        let at = open(files, v, "home/seq");
        for k in 0..PAGES as u64 {
            assert_eq!(read(files, v, at, k * PAGE, PAGE as usize).len(), PAGE as usize);
        }
        files.closed(v, at.1, at.0).unwrap();
        fake.0.borrow().supplies - before
    };
    assert!(round(&mut files, &mut v) > 0);
    let again: Vec<u32> = (0..3).map(|_| round(&mut files, &mut v)).collect();
    assert_eq!(again, [0; 3], "supplies each round after the first");
}

/// The pager serves a bounded count of objects: a refused create makes room
/// by letting a file nobody holds go, and an open one is kept.
#[test]
fn an_object_refused_for_room_is_made_by_a_closed_files_going() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    fake.0.borrow_mut().objects = 2;
    let mut files = Files::new(Some(fake.clone()));
    let a = open(&mut files, &mut v, "home/a");
    let b = open(&mut files, &mut v, "home/b");
    files.closed(&mut v, b.1, b.0).unwrap();
    let c = open(&mut files, &mut v, "home/c");
    assert!(files.object(c.1).is_some(), "the new file has its object");
    assert!(files.object(b.1).is_none(), "the closed file's went");
    assert!(files.object(a.1).is_some(), "the open file's stays");
    let n = v.open("home/d", CREATE).unwrap();
    let identity = v.identity("home/d").unwrap();
    assert_eq!(files.opened(&mut v, identity, n, RECORD), Err(Error::NoSpace), "both held open: refused, by name");
}

/// XSH 2.9.7, the server's half: a write that takes many calls — an absent
/// page supplied, the dirty ceiling written back, a last page in part — is
/// one write of the object's, so every state a holder can read between calls
/// is the file before it or after it, and the write ends whether it lands
/// or is refused.
#[test]
fn a_write_of_many_calls_is_read_whole_or_not_at_all() {
    const PAGES: usize = 8;
    let page = PAGE as usize;
    let old = pattern(PAGES * page, 10);
    let mut v = volume();
    let n = v.open("home/f", CREATE).unwrap();
    v.write(n, 0, &old).unwrap();
    v.close(n).unwrap();
    let fake = Fake::new(64, 4);
    let mut files = Files::new(Some(fake.clone()));
    let at = open(&mut files, &mut v, "home/f");
    read(&mut files, &mut v, at, 0, PAGES * page);
    let object = files.object(at.1).unwrap();
    assert_eq!(fake.withdraw(object, 3, 1), Ok(1));
    fake.0.borrow_mut().views.clear();
    let supplies = fake.0.borrow().supplies;

    let new = pattern(6 * page + 100, 11);
    files.write(&mut v, at.1, at.0, Some(0), &new).unwrap();
    let mut after = old.clone();
    after[..new.len()].copy_from_slice(&new);
    let m = fake.0.borrow();
    assert!(m.holding.is_empty(), "the write's hold was not given back");
    assert!(m.supplies > supplies, "the write supplied no absent page: one call, nothing to tear");
    for (k, (_, view)) in m.views.iter().filter(|(id, _)| *id == object).enumerate() {
        let as_of = |file: &[u8]| view.iter().all(|(&p, bytes)| p as usize >= PAGES || bytes[..] == file[p as usize * page..][..page]);
        assert!(as_of(&old) || as_of(&after), "view {k} of {} is neither the file before the write nor after it", m.views.len());
    }
    drop(m);

    fake.0.borrow_mut().refusing = true;
    assert!(files.write(&mut v, at.1, at.0, Some(7 * page as u64 + 50), &pattern(3 * page, 12)).is_err());
    assert!(fake.0.borrow().holding.is_empty(), "a refused write's hold was not given back");
}

/// A volume whose every block holds what an earlier tenant left, 0xAB.
fn stale_volume() -> DataVolume<Ram> {
    let disk = Ram::new(16384);
    let stale = vec![[0xAB; toyos_blockdev::BLOCK]; 16384];
    toyos_blockdev::block_on(toyos_blockdev::BlockDev::write(&disk, 0, &stale), || ()).unwrap();
    DataVolume::format(disk, 256, &["home"], clock).unwrap()
}

/// `v` stopped now and mounted again: what its last commit left.
fn after_a_stop(v: DataVolume<Ram>) -> DataVolume<Ram> {
    match DataVolume::probe(v.stopped(), 256, &["home"], clock) {
        crate::data::Probed::Mounted(v) => *v,
        _ => panic!("the volume does not mount after a stop"),
    }
}

/// An fsync makes its own file durable, and leaves another file's write
/// over blocks the volume already named in the object, unwritten: nothing
/// the commit makes durable names those blocks anew.
#[test]
fn an_fsync_makes_its_file_durable_and_no_other_files_overwrite() {
    let page = PAGE as usize;
    let mut v = stale_volume();
    let mut files = Files::new(Some(Fake::new(64, 16)));
    let b = open(&mut files, &mut v, "home/b");
    files.write(&mut v, b.1, b.0, Some(0), &pattern(2 * page, 1)).unwrap();
    assert_eq!(files.sync(&mut v).unwrap(), []);
    let a = open(&mut files, &mut v, "home/a");
    files.write(&mut v, a.1, a.0, Some(0), &pattern(2 * page, 2)).unwrap();
    files.write(&mut v, b.1, b.0, Some(0), &pattern(2 * page, 3)).unwrap();

    files.fsync(&mut v, Some((a.1, a.0))).unwrap();
    assert!(files.files[&b.1].dirty.is_some(), "the other file's overwrite was written back");
    assert!(files.owes(&v), "the other file's overwrite is owed to a later sync");
    let mut v = after_a_stop(v);
    assert!(on_volume(&mut v, "home/a") == pattern(2 * page, 2), "the fsynced file is not as written");
    assert!(on_volume(&mut v, "home/b") == pattern(2 * page, 1), "the other file's overwrite reached the volume");
}

/// A file whose blocks the commit holds as taken is carried by an fsync of
/// another: its pages reach the volume first and its entry goes with them,
/// so after a stop it reads its own bytes, never an earlier tenant's.
#[test]
fn an_fsync_commits_its_file_without_publishing_another_files_dirty_size() {
    let page = PAGE as usize;
    let mut v = stale_volume();
    let mut files = Files::new(Some(Fake::new(64, 16)));
    let a = open(&mut files, &mut v, "home/a");
    let c = open(&mut files, &mut v, "home/c");
    files.write(&mut v, a.1, a.0, Some(0), &pattern(page, 4)).unwrap();
    files.write(&mut v, c.1, c.0, Some(0), &pattern(3 * page, 5)).unwrap();

    files.fsync(&mut v, Some((a.1, a.0))).unwrap();
    assert!(files.owes(&v), "the other file still has buffered data");
    let mut v = after_a_stop(v);
    assert!(on_volume(&mut v, "home/a") == pattern(page, 4), "the fsynced file is not as written");
    let carried = on_volume(&mut v, "home/c");
    assert!(carried.is_empty(), "unwritten data must not become a durable file length");
}

/// A file whose pages a sync could not write is owed after an fsync of
/// another, though that sync made its entry durable and the commit does not
/// carry it.
#[test]
fn a_file_a_sync_could_not_write_back_stays_owed_past_another_s_fsync() {
    let page = PAGE as usize;
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let a = open(&mut files, &mut v, "home/a");
    let f = open(&mut files, &mut v, "home/f");
    files.write(&mut v, f.1, f.0, Some(0), &pattern(page, 6)).unwrap();
    fake.0.borrow_mut().unwritable = Some(files.files[&f.1].object);
    assert_eq!(files.sync(&mut v).map(|u| u.len()), Ok(1), "the sync wrote the refused file's pages");
    assert!(v.owes(), "the failed file's buffered metadata must stay pending");
    files.write(&mut v, a.1, a.0, Some(0), &pattern(page, 7)).unwrap();

    files.fsync(&mut v, Some((a.1, a.0))).unwrap();
    assert!(files.owes(&v), "f's unwritten pages are owed to a later sync");
}

/// A directory's fsync commits the names every change already wrote.
#[test]
fn a_directorys_fsync_makes_a_new_name_durable() {
    let mut v = stale_volume();
    let mut files = Files::new(Some(Fake::new(64, 16)));
    v.mkdir("home/d").unwrap();
    files.fsync(&mut v, None).unwrap();
    let mut v = after_a_stop(v);
    assert_eq!(v.lstat("home/d").map(|m| m.kind), Ok(crate::volume::Kind::Dir));
}

/// A holder stops at the volume's size after an open, write, refused write
/// or truncate.
#[test]
fn an_objects_size_is_the_volumes_after_every_change() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let n = v.open("home/f", CREATE).unwrap();
    v.write(n, 0, b"before").unwrap();
    v.close(n).unwrap();
    let (node, id) = open(&mut files, &mut v, "home/f");
    let object = files.object(id).unwrap();
    let published = |v: &mut DataVolume<Ram>, what: &str| {
        let m = fake.0.borrow();
        let meta = v.node_meta(node).unwrap();
        assert_eq!(m.sizes[&object], meta.size, "after {what}");
    };
    published(&mut v, "the open");
    files.write(&mut v, id, node, Some(3 * PAGE), b"after").unwrap();
    published(&mut v, "a write");
    fake.0.borrow_mut().refusing = true;
    assert!(files.write(&mut v, id, node, Some(9 * PAGE), &pattern(2 * PAGE as usize, 1)).is_err());
    fake.0.borrow_mut().refusing = false;
    published(&mut v, "a refused write");
    files.truncate(&mut v, id, node, 100).unwrap();
    published(&mut v, "a truncate");
}

/// A file a holder may write on DATA: room for a bulk of pages is held on
/// the volume and granted as credit. What the holder writes, growing the
/// file past a hole, is the file's at once, size and time the object's, and
/// reaches the volume at the sync, from the pool.
#[test]
fn a_holders_writes_reach_the_volume_from_the_pool_at_the_sync() {
    let mut v = volume();
    let fake = Fake::new(256, 128);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    let room = v.room().unwrap().free;
    files.held(&mut v, id, true);
    let object = files.object(id).unwrap();
    assert_eq!(fake.status(object).unwrap().credit, BULK);
    assert!(v.room().unwrap().free < room, "the bulk's room is held");
    let head = pattern(3 * PAGE as usize + 7, 6);
    assert_eq!(fake.holder_write(object, 0, &head, 11), Some(head.len()));
    assert_eq!(fake.holder_write(object, 9 * PAGE, b"past a hole", 12), Some(11));
    let size = 9 * PAGE + 11;
    let meta = files.meta(id, v.node_meta(node).unwrap());
    assert_eq!((meta.size, meta.mtime), (size, 12), "the object's, ahead of the volume's");
    assert!(files.notice(&mut v, Notice::Dirtied { object }), "a write-back is owed");
    assert_eq!(files.sync(&mut v).unwrap(), []);
    let mut want = vec![0u8; size as usize];
    want[..head.len()].copy_from_slice(&head);
    want[9 * PAGE as usize..].copy_from_slice(b"past a hole");
    assert_eq!(on_volume(&mut v, "home/f"), want);
    assert_eq!(v.node_meta(node).unwrap().mtime, 12, "the volume took the object's time");
}

/// A page a holder dirtied before its notice is read is written back by an
/// fsync of the file all the same: the server takes what holders dirtied
/// from the object, not from the notices it has read.
#[test]
fn an_fsync_writes_back_what_a_holder_dirtied_before_its_notice_is_read() {
    let mut v = volume();
    let fake = Fake::new(256, 128);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.held(&mut v, id, true);
    let object = files.object(id).unwrap();
    assert_eq!(fake.holder_write(object, 0, b"durable", 1), Some(7));
    files.fsync(&mut v, Some((id, node))).unwrap();
    assert_eq!(on_volume(&mut v, "home/f"), b"durable");
}

/// The credit holders did not spend is room again when the last holder
/// goes; what the pool held for a page they dirtied, once it is written.
#[test]
fn the_room_held_for_holders_comes_back_once_none_can_write() {
    let mut v = volume();
    let fake = Fake::new(256, 128);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    let room = v.room().unwrap().free;
    files.held(&mut v, id, true);
    let object = files.object(id).unwrap();
    assert!(!files.notice(&mut v, Notice::NoHolders { object }), "nothing was written");
    assert_eq!((fake.status(object).unwrap().credit, v.room().unwrap().free), (0, room), "taken back whole");
    files.held(&mut v, id, true);
    assert_eq!(fake.holder_write(object, 0, &[7; 100], 2), Some(100));
    assert!(files.notice(&mut v, Notice::NoHolders { object }), "the page it dirtied is owed");
    assert_eq!(fake.status(object).unwrap().credit, 0);
    assert_eq!(files.sync(&mut v).unwrap(), []);
    assert_eq!(on_volume(&mut v, "home/f"), [7; 100]);
    assert!(files.object(id).is_some() && v.node_meta(node).is_ok());
}

/// A holder's write the credit cannot cover is refused whole, and the
/// server's own write of it makes the credit up again.
#[test]
fn a_write_past_the_credit_goes_to_the_server_which_makes_it_up() {
    let mut v = volume();
    let fake = Fake::new(512, 256);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.held(&mut v, id, true);
    let object = files.object(id).unwrap();
    let data = pattern((BULK as usize + 1) * PAGE as usize, 7);
    assert_eq!(fake.holder_write(object, 0, &data, 1), None, "a page more than the credit");
    assert_eq!(fake.status(object).unwrap().credit, BULK, "a refusal spends nothing");
    assert_eq!(fake.holder_write(object, 0, &data[..PAGE as usize], 2), Some(PAGE as usize));
    files.write(&mut v, id, node, Some(0), &data).unwrap();
    assert_eq!(fake.status(object).unwrap().credit, BULK, "made up again");
    assert_eq!(files.sync(&mut v).unwrap(), []);
    assert_eq!(on_volume(&mut v, "home/f"), data);
}

/// In memory a holder writes without credit, and the file is what the
/// object holds, its size and time the object's.
#[test]
fn a_holder_writes_a_memory_file_without_credit() {
    let mut volume = crate::memory::MemoryVolume::new(&[], clock);
    let fake = Fake::new(8, 0);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut volume, "file");
    files.held(&mut volume, id, true);
    let object = files.object(id).unwrap();
    assert_eq!(fake.status(object).unwrap().credit, 0, "memory grants nothing");
    assert_eq!(fake.holder_write(object, 2 * PAGE, b"tail", 9), Some(4));
    let meta = files.meta(id, volume.node_meta(node).unwrap());
    assert_eq!((meta.size, meta.mtime), (2 * PAGE + 4, 9));
    assert_eq!(read(&mut files, &mut volume, (node, id), 2 * PAGE, 8), b"tail");
    assert!(!files.notice(&mut volume, Notice::Dirtied { object }), "nothing to write back");
    assert!(!files.notice(&mut volume, Notice::NoHolders { object }));
    let meta = volume.node_meta(node).unwrap();
    assert_eq!((meta.size, meta.mtime), (2 * PAGE + 4, 9), "the volume took the object's once no holder can write");
}

/// Pages holders dirtied count against the dirty ceiling, so a server write
/// that meets it writes them back to make room, whether or not a notice of
/// them was read, rather than answer that the volume is full.
#[test]
fn a_server_write_at_the_ceiling_writes_back_what_holders_dirtied() {
    let mut v = volume();
    let fake = Fake::new(64, 4);
    let mut files = Files::new(Some(fake.clone()));
    for (k, path) in ["home/a", "home/b"].into_iter().enumerate() {
        let (_, id) = open(&mut files, &mut v, path);
        files.held(&mut v, id, true);
        let object = files.object(id).unwrap();
        assert_eq!(fake.holder_write(object, 0, &pattern(2 * PAGE as usize, k as u8), 1), Some(2 * PAGE as usize));
    }
    assert_eq!(fake.0.borrow().held(), 4, "holders' pages alone reach the ceiling");
    let (node, id) = open(&mut files, &mut v, "home/c");
    assert_eq!(files.write(&mut v, id, node, Some(0), b"room"), Ok(4));
    assert_eq!(on_volume(&mut v, "home/a"), pattern(2 * PAGE as usize, 0), "written back to make room");
}

/// A server write holds the object's writes for its whole: a holder that
/// asks to write meanwhile is refused and asks the server, so a write the
/// server then cuts back, refused part way, cuts back only its own bytes.
#[test]
fn a_refused_server_write_holds_holders_off_and_cuts_back_only_its_own() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.held(&mut v, id, true);
    let object = files.object(id).unwrap();
    assert_eq!(fake.holder_write(object, 0, b"holder's", 1), Some(8));
    {
        let mut m = fake.0.borrow_mut();
        m.during_write = Some((object, 3 * PAGE, b"grown by a holder".to_vec()));
        m.refusing = true;
    }
    assert!(files.write(&mut v, id, node, Some(PAGE), &pattern(2 * PAGE as usize, 6)).is_err());
    assert_eq!(fake.0.borrow().during_answer, Some(None), "the holder's write meanwhile was refused");
    fake.0.borrow_mut().refusing = false;
    assert_eq!(fake.status(object).unwrap().size, 8, "the size the holder left");
    assert_eq!(read(&mut files, &mut v, (node, id), 0, 100), b"holder's");
}

/// An append through the server lands at the end the object holds, past
/// what a holder wrote, and answers the offset after it.
#[test]
fn an_append_through_the_server_lands_at_the_end_holders_left() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/log");
    files.held(&mut v, id, true);
    let object = files.object(id).unwrap();
    assert_eq!(fake.holder_write(object, 0, b"first line\n", 1), Some(11));
    assert_eq!(files.write(&mut v, id, node, None, b"second\n"), Ok(18));
    assert_eq!(read(&mut files, &mut v, (node, id), 0, 100), b"first line\nsecond\n");
}

/// Where the server writes a page a holder's credit paid for, that credit
/// is the holders' again: making the credit up after the write adds no room.
#[test]
fn a_page_the_server_writes_gives_its_holders_credit_back() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.held(&mut v, id, true);
    let object = files.object(id).unwrap();
    assert_eq!(fake.holder_write(object, 0, &pattern(PAGE as usize, 1), 1), Some(PAGE as usize));
    assert_eq!(fake.status(object).unwrap().credit, BULK - 1);
    files.write(&mut v, id, node, Some(0), &pattern(PAGE as usize, 2)).unwrap();
    assert_eq!(fake.status(object).unwrap().credit, BULK, "the page the server wrote paid back");
    assert_eq!(fake.0.borrow().granted, BULK, "and nothing more was granted, nor room held for it");
}

/// The server stamps the time of its write, and of its truncation, inside
/// its hold: no holder states the size it left with the time before it.
#[test]
fn a_server_write_stamps_its_time_inside_its_hold() {
    let mut v = volume();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    fake.0.borrow_mut().last_stamp_held = None;
    files.write(&mut v, id, node, Some(0), b"stamped").unwrap();
    assert_eq!(fake.0.borrow().last_stamp_held, Some(true), "the write's");
    fake.0.borrow_mut().last_stamp_held = None;
    files.truncate(&mut v, id, node, 3).unwrap();
    assert_eq!(fake.0.borrow().last_stamp_held, Some(true), "the truncation's");
}
