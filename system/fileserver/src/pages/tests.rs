use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use toyos_abi::syscall::SyscallError;

use super::*;
use crate::data::DataVolume;
use crate::ram::Ram;
use crate::volume::OpenHow;

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
    supplies: u32,
    /// Every supply from here is refused.
    refusing: bool,
    /// Objects a call that did not take all it was given left written.
    held: BTreeSet<u64>,
    /// Each object's pages as a holder could read them after each change
    /// that left no write under way.
    views: Vec<(u64, BTreeMap<u64, Vec<u8>>)>,
}

struct Page {
    bytes: Vec<u8>,
    dirty: bool,
    writeback: bool,
}

impl Model {
    fn held(&self) -> usize {
        self.pages.values().filter(|p| p.dirty || p.writeback).count()
    }

    /// A change to `id` ended, `whole` where the call took all it was given:
    /// the kernel's rule (`kernel::paged::Object::write_end`).
    fn changed(&mut self, id: u64, whole: bool) {
        if !whole {
            self.held.insert(id);
        }
        if !self.held.contains(&id) {
            let view = self.pages.iter().filter(|((c, _), _)| *c == id).map(|(&(_, p), page)| (p, page.bytes.clone())).collect();
            self.views.push((id, view));
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
                continue;
            } else if m.pages.len() >= m.capacity {
                Some(Refused::NoRoom)
            } else if dirty && m.held() >= m.ceiling {
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
            m.pages.insert((id, page), Page { bytes: held, dirty, writeback: false });
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
        let mut done = 0;
        while done < bytes.len() {
            let at = offset + done as u64;
            let key = (id, at / PAGE);
            let Some(page) = m.pages.get(&key) else { break };
            if !page.dirty && !page.writeback && m.held() >= m.ceiling {
                return if done > 0 { Ok(done) } else { Err(Refused::DirtyFull) };
            }
            let within = (at % PAGE) as usize;
            let n = (PAGE as usize - within).min(bytes.len() - done);
            let page = m.pages.get_mut(&key).unwrap();
            page.bytes[within..within + n].copy_from_slice(&bytes[done..done + n]);
            page.dirty = true;
            let s = m.sizes.get_mut(&id).unwrap();
            *s = (*s).max(at + n as u64);
            done += n;
        }
        if done == 0 { Err(Refused::Absent) } else { Ok(done) }
    }
}

impl Pager for Fake {
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
            let Some(page) = m.pages.get(&(id, at / PAGE)) else { break };
            let within = (at % PAGE) as usize;
            let n = ((PAGE as usize - within) as u64).min(end - at) as usize;
            out.write((at - offset) as usize, &page.bytes[within..within + n]);
            at += n as u64;
        }
        match at - offset {
            0 if offset < end => Err(Refused::Absent),
            n => Ok(n as usize),
        }
    }

    fn supply(&self, id: u64, offset: u64, bytes: &[u8], dirty: bool) -> Result<u64, Refused> {
        let answer = self.supply_pages(id, offset, bytes, dirty);
        if dirty {
            self.0.borrow_mut().changed(id, answer == Ok(bytes.len().div_ceil(PAGE as usize) as u64));
        }
        answer
    }

    fn write(&self, id: u64, offset: u64, bytes: &[u8]) -> Result<usize, Refused> {
        let answer = self.write_pages(id, offset, bytes);
        self.0.borrow_mut().changed(id, answer == Ok(bytes.len()));
        answer
    }

    fn end_write(&self, id: u64) -> Result<(), Refused> {
        let mut m = self.0.borrow_mut();
        assert!(m.held.remove(&id), "a write ended that no short call left under way");
        m.changed(id, true);
        Ok(())
    }

    fn withdraw(&self, id: u64, first: u64, count: u64) -> Result<u64, Refused> {
        let mut m = self.0.borrow_mut();
        let before = m.pages.len();
        let end = first.saturating_add(count);
        m.pages.retain(|(c, p), page| *c != id || !(first..end).contains(p) || page.dirty || page.writeback);
        Ok((before - m.pages.len()) as u64)
    }

    fn fail(&self, id: u64, first: u64, count: u64) -> Result<(), Refused> {
        self.0.borrow_mut().pages.retain(|(c, p), _| *c != id || !(first..first + count).contains(p));
        Ok(())
    }

    fn resize(&self, id: u64, size: u64) -> Result<(), Refused> {
        let mut m = self.0.borrow_mut();
        m.pages.retain(|(c, p), _| *c != id || *p < size.div_ceil(PAGE));
        if let Some(page) = m.pages.get_mut(&(id, size / PAGE)) {
            page.bytes[(size % PAGE) as usize..].fill(0);
        }
        m.sizes.insert(id, size);
        m.changed(id, true);
        Ok(())
    }

    fn writeback_begin(&self, id: u64, first: u64, count: u64) -> Result<[u64; 8], Refused> {
        let mut bits = [0u64; 8];
        for (&(c, p), page) in self.0.borrow_mut().pages.iter_mut() {
            if c == id && (first..first + count).contains(&p) && page.dirty {
                page.dirty = false;
                page.writeback = true;
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
    DataVolume::format(Ram::new(4096), 256, &["home"], clock).unwrap()
}

const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };

/// `path` opened through `files`: its node and identity.
fn open(files: &mut Files<Fake>, v: &mut DataVolume<Ram>, path: &str) -> (Node, u64) {
    let node = v.open(path, CREATE).unwrap();
    let identity = v.identity(path).unwrap();
    files.opened(v, identity, node).unwrap();
    (node, identity)
}

fn read(files: &mut Files<Fake>, v: &mut DataVolume<Ram>, (node, id): (Node, u64), offset: u64, len: usize) -> Vec<u8> {
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
    files.write(&mut v, id, node, 5000, b"new bytes over the end of the file").unwrap();
    files.write(&mut v, id, node, 5990, b"and past it").unwrap();
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
    files.write(&mut v, id, node, 0, &data).unwrap();
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
    files.write(&mut v, id, node, 0, &pattern(3 * PAGE as usize, 5)).unwrap();
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
    files.write(&mut v, id, node, 0, b"kept").unwrap();
    fake.0.borrow_mut().refusing = true;
    assert!(files.write(&mut v, id, node, PAGE, &pattern(3 * PAGE as usize, 6)).is_err());
    assert_eq!(v.node_meta(node).unwrap().size, 4, "the volume's size is back where the object's is");
    fake.0.borrow_mut().refusing = false;
    files.sync(&mut v).unwrap();
    assert_eq!(on_volume(&mut v, "home/f"), b"kept");
}

/// A write into a hole the file was extended over, refused before any byte
/// landed: the block its reserve took there goes back, though the hole lies
/// inside the old size, and the hole still reads zeros.
#[test]
fn a_write_refused_in_a_hole_gives_back_the_block_it_took() {
    // Every block the volume does not write holds what an earlier tenant left.
    let disk = Ram::new(4096);
    let stale = vec![[0xAB; toyos_blockdev::BLOCK]; 4096];
    toyos_blockdev::block_on(toyos_blockdev::BlockDev::write(&disk, 0, &stale), || ()).unwrap();
    let mut v = DataVolume::format(disk, 256, &["home"], clock).unwrap();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, 0, &pattern(PAGE as usize, 4)).unwrap();
    files.truncate(&mut v, id, node, 3 * PAGE).unwrap();
    files.sync(&mut v).unwrap();
    let room = v.room().unwrap().free;
    fake.0.borrow_mut().refusing = true;
    assert!(files.write(&mut v, id, node, PAGE, &pattern(PAGE as usize, 5)).is_err());
    assert_eq!(v.room().unwrap().free, room, "the refused write kept the hole's block, or gave back more");
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
    let disk = Ram::new(4096);
    let stale = vec![[0xAB; toyos_blockdev::BLOCK]; 4096];
    toyos_blockdev::block_on(toyos_blockdev::BlockDev::write(&disk, 0, &stale), || ()).unwrap();
    let mut v = DataVolume::format(disk, 256, &["home"], clock).unwrap();
    let fake = Fake::new(64, 16);
    let mut files = Files::new(Some(fake.clone()));
    let (node, id) = open(&mut files, &mut v, "home/f");
    files.write(&mut v, id, node, 0, &pattern(PAGE as usize, 4)).unwrap();
    files.truncate(&mut v, id, node, 3 * PAGE).unwrap();
    files.sync(&mut v).unwrap();
    files.write(&mut v, id, node, 6000, &[2]).unwrap();

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
    files.write(&mut v, id, node, 0, &pattern(2 * PAGE as usize, 8)).unwrap();
    
    let truncate = OpenHow { create: false, create_new: false, truncate: true };
    let again = v.open("home/f", truncate).unwrap();
    files.truncated(id).unwrap();
    files.opened(&mut v, id, again).unwrap();
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
    assert_eq!(files.opened(&mut v, identity, n), Err(Error::NoSpace), "both held open: refused, by name");
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
    files.write(&mut v, at.1, at.0, 0, &new).unwrap();
    let mut after = old.clone();
    after[..new.len()].copy_from_slice(&new);
    let m = fake.0.borrow();
    assert!(m.held.is_empty(), "the write was left under way");
    assert!(m.supplies > supplies, "the write supplied no absent page: one call, nothing to tear");
    for (k, (_, view)) in m.views.iter().filter(|(id, _)| *id == object).enumerate() {
        let as_of = |file: &[u8]| view.iter().all(|(&p, bytes)| p as usize >= PAGES || bytes[..] == file[p as usize * page..][..page]);
        assert!(as_of(&old) || as_of(&after), "view {k} of {} is neither the file before the write nor after it", m.views.len());
    }
    drop(m);

    fake.0.borrow_mut().refusing = true;
    assert!(files.write(&mut v, at.1, at.0, 7 * page as u64 + 50, &pattern(3 * page, 12)).is_err());
    assert!(fake.0.borrow().held.is_empty(), "a refused write was left under way");
}

/// A volume whose every block holds what an earlier tenant left, 0xAB.
fn stale_volume() -> DataVolume<Ram> {
    let disk = Ram::new(4096);
    let stale = vec![[0xAB; toyos_blockdev::BLOCK]; 4096];
    toyos_blockdev::block_on(toyos_blockdev::BlockDev::write(&disk, 0, &stale), || ()).unwrap();
    DataVolume::format(disk, 256, &["home"], clock).unwrap()
}

/// `v` stopped now and mounted again: what its last commit left.
fn after_a_stop(v: DataVolume<Ram>) -> DataVolume<Ram> {
    match DataVolume::probe(v.stopped(), 256, &["home"], clock) {
        crate::data::Probed::Mounted(v) => v,
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
    files.write(&mut v, b.1, b.0, 0, &pattern(2 * page, 1)).unwrap();
    assert_eq!(files.sync(&mut v).unwrap(), []);
    let a = open(&mut files, &mut v, "home/a");
    files.write(&mut v, a.1, a.0, 0, &pattern(2 * page, 2)).unwrap();
    files.write(&mut v, b.1, b.0, 0, &pattern(2 * page, 3)).unwrap();

    files.fsync(&mut v, Some((a.1, a.0))).unwrap();
    assert!(files.files[&b.1].dirty.is_some(), "the other file's overwrite was written back");
    let mut v = after_a_stop(v);
    assert!(on_volume(&mut v, "home/a") == pattern(2 * page, 2), "the fsynced file is not as written");
    assert!(on_volume(&mut v, "home/b") == pattern(2 * page, 1), "the other file's overwrite reached the volume");
}

/// A file whose blocks the commit holds as taken is carried by an fsync of
/// another: its pages reach the volume first and its entry goes with them,
/// so after a stop it names no block that holds an earlier tenant's bytes,
/// and no block is held that nothing names.
#[test]
fn an_fsync_writes_first_the_pages_of_every_file_its_commit_carries() {
    let page = PAGE as usize;
    let mut v = stale_volume();
    let mut files = Files::new(Some(Fake::new(64, 16)));
    let a = open(&mut files, &mut v, "home/a");
    let c = open(&mut files, &mut v, "home/c");
    files.write(&mut v, a.1, a.0, 0, &pattern(page, 4)).unwrap();
    files.write(&mut v, c.1, c.0, 0, &pattern(3 * page, 5)).unwrap();

    files.fsync(&mut v, Some((a.1, a.0))).unwrap();
    let mut v = after_a_stop(v);
    assert!(on_volume(&mut v, "home/a") == pattern(page, 4), "the fsynced file is not as written");
    let carried = on_volume(&mut v, "home/c");
    assert!(carried == pattern(3 * page, 5), "the carried file reads {} bytes, not its own", carried.len());
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
