//! One disk's sessions, as its driver serves them.
//!
//! **A session is a partition held**, from the open the block service hands
//! over to the client's leaving, which its doorbell's end says: one holder per partition
//! (`toyos-blockhold`), every request bounded to the partition and the arena
//! before the device sees it (`toyos_blockring::server`), the data moved by
//! the device straight into and out of the client's own region, which the
//! kernel maps into the controller's domain for the length of the session
//! (`SYS_DEVICE_DMA_MAP`) and takes back at its end — after the last command
//! naming it has been answered, never before.
//!
//! **A server never blocks on a client.** The open is answered with one
//! `try_send` on the client's own connection, carrying the session's
//! doorbells, which the driver makes; a ring never waits.
//!
//! **A designation is the driver's own work on one partition** ([`Job`]): it
//! holds the partition as a session would for as long as it runs, so nothing
//! opens it meanwhile, and it reaches the device through the same [`Device`]
//! as every session. What it writes is the stamp alone, which it makes
//! itself; no client's bytes reach the disk through it.

use std::collections::BTreeMap;

use toyos::ipc::Connection;
use toyos::poller::{Poller, READABLE};
use toyos::wake::{self, Bell, Waker};
use toyos_abi::syscall::SyscallError;
use toyos_blockhold::Holds;
use toyos_blockring::disk::Part;
use toyos_blockring::entry::{Completion, Op};
use toyos_blockring::layout::{ServerRings, DEPTH};
use toyos_blockring::server::{ServerSession, Taken};
use toyos_blockring::disk::Designation;
use toyos_blockring::wire::{self, Designated, DiskHead, Opened, Refusal, SlotLine};
use toyos_blockring::Status;

use crate::designate::{self, Ask};
use crate::region::Region;
use crate::table::{self, Read};
use crate::Partition;

/// Sessions one disk holds at once. Each costs its region's mapping in the
/// driver and in the controller's domain, which the kernel bounds per claim.
pub const MAX_SESSIONS: usize = 8;

/// A request of a session, as its device answers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub session: u64,
    pub tag: u32,
    pub write: bool,
}

/// What a disk's sessions ask of the controller it is on.
pub trait Device {
    /// Whether one more command can be issued now.
    fn has_room(&self) -> bool;
    /// Issue a read or a write of `blocks` blocks at the disk's block
    /// `block`, to or from device address `at`, answered for `owner`.
    fn submit_io(&mut self, write: bool, block: u64, blocks: u32, at: u64, owner: Owner);
    /// Issue a flush of the disk's volatile cache for `owner`; `false` for a
    /// disk with none, whose every write answered is on the medium already.
    fn submit_flush(&mut self, owner: Owner) -> bool;
    /// Map `region` into the controller's domain, answering its device
    /// address.
    fn dma_map(&mut self, region: toyos::BorrowedHandle<'_>) -> Result<u64, SyscallError>;
    fn dma_unmap(&mut self, device_addr: u64) -> Result<(), SyscallError>;
}

/// A partition held, over a client's region.
struct Served {
    /// Rung by the client for its requests; its waker going is the client's leaving.
    heard: Bell,
    /// Rung for the client's answers.
    ring: Waker,
    region: Region,
    device_addr: u64,
    rings: ServerRings,
    state: ServerSession,
    unique: [u8; 16],
    /// The client left, or broke the protocol: nothing more is taken, and
    /// the session ends once nothing of it is on the device.
    closing: bool,
    requests: u64,
    posted: bool,
}

/// The session id the driver's own work answers under: no session is ever
/// numbered so.
const JOB: u64 = u64::MAX;

/// A designation in progress, on a partition it holds.
struct Job {
    conn: Connection,
    order: Designation,
    steps: designate::Designation,
    region: Region,
    device_addr: u64,
    first: u64,
}

impl designate::Memory for Region {
    fn copy_out(&self, at: usize, out: &mut [u8]) {
        self.window().copy_out(at, out);
    }
    fn copy_in(&mut self, at: usize, data: &[u8]) {
        self.window().copy_in(at, data);
    }
}

/// Every session on one disk, and the partitions its table names.
pub struct Sessions {
    /// Who says this disk's lines.
    name: String,
    parts: Vec<Partition>,
    read: Read,
    holds: Holds<u64>,
    /// The device's loss count: every reset may have dropped its cache.
    losses: u64,
    sessions: BTreeMap<u64, Served>,
    next_id: u64,
    job: Option<Job>,
}

impl Sessions {
    /// The disk `read` describes.
    pub fn new(name: String, read: Read) -> Self {
        let parts = table::say(&name, &read);
        Self {
            name,
            parts,
            read,
            holds: Holds::new(),
            losses: 0,
            sessions: BTreeMap::new(),
            next_id: 0,
            job: None,
        }
    }

    /// What the block service is offered of this disk.
    pub fn offered(&self) -> Vec<Part> {
        self.parts
            .iter()
            .map(|p| Part { unique: p.unique, kind: p.kind, blocks: p.span.as_ref().ok().map(|&(_, n)| n) })
            .collect()
    }

    /// The span a session on `unique` is served on, held for it; or its
    /// refusal. The service admitted the open against this disk's offer, so
    /// what is refused here is what only the disk knows.
    ///
    /// **A client that hung up holds nothing**: every session's end its client
    /// has already made is read before the open is judged.
    pub fn place(&mut self, unique: [u8; 16], dev: &mut impl Device) -> Result<(u64, u64), Refusal> {
        let span = match self.parts.iter().find(|p| p.unique == unique && unique != [0; 16]) {
            None => return Err(Refusal::NotFound),
            Some(Partition { span: Err(_), .. }) => return Err(Refusal::Unusable),
            Some(Partition { span: Ok(span), .. }) => *span,
        };
        let ids: Vec<u64> = self.sessions.keys().copied().collect();
        for id in ids {
            self.hear(id);
        }
        self.retire(dev);
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(Refusal::Exhausted);
        }
        let (first, blocks) = span;
        self.holds.hold(first, first + blocks, self.next_id).map_err(|_| Refusal::Held)?;
        Ok(span)
    }

    /// Serve the session the service handed over: answer the client's open on
    /// its own connection, or refuse it there. `region` is the client's and
    /// is consumed either way.
    pub fn open(&mut self, dev: &mut impl Device, conn: Connection, region: toyos::OwnedHandle, unique: [u8; 16], writes: bool) {
        let name = self.name.clone();
        let refuse = |conn: &Connection, why: Refusal| {
            println!("{name}: an open of {} refused: {why:?}", crate::guid_text(unique));
            let _ = conn.try_send_bytes(wire::MSG_REFUSED, &why.encode());
        };
        // A region the kernel did not allocate, or longer than a session,
        // which would spend the claim's bound on the kernel's side for every
        // other client, is refused whole.
        let Ok(region) = Region::lent(region) else { return refuse(&conn, Refusal::Malformed) };
        let (first, blocks) = match self.place(unique, dev) {
            Ok(span) => span,
            Err(why) => return refuse(&conn, why),
        };
        let device_addr = match dev.dma_map(region.handle()) {
            Ok(at) => at,
            Err(why) => {
                self.holds.release(first);
                return refuse(&conn, if why == SyscallError::ResourceExhausted { Refusal::Exhausted } else { Refusal::Malformed });
            }
        };
        let bells = wake::pair().and_then(|requests| Ok((requests, wake::pair()?)));
        let Ok(((client_ring, heard), (ring, answered))) = bells else {
            if let Err(why) = dev.dma_unmap(device_addr) {
                panic!("{}: the kernel would not take a refused session's region back: {why:?}", self.name);
            }
            self.holds.release(first);
            return refuse(&conn, Refusal::Exhausted);
        };
        let (rings, opened) = Opened::over(region.words(), blocks, unique);
        if conn.try_send_bytes_with(wire::MSG_OPENED, &opened.encode(), [client_ring.into(), answered.into()]).is_err() {
            // The client went before it heard: nothing of the session reached
            // the device, and what was taken for it goes back.
            if let Err(why) = dev.dma_unmap(device_addr) {
                panic!("{}: the kernel would not take an abandoned session's region back: {why:?}", self.name);
            }
            return self.holds.release(first);
        }
        let id = self.next_id;
        self.next_id += 1;
        let state = ServerSession::new(first, opened.blocks(), writes);
        let served = Served { heard, ring, region, device_addr, rings, state, unique, closing: false, requests: 0, posted: false };
        self.sessions.insert(id, served);
        let access = if writes { "" } else { ", read-only" };
        println!("{}: session {id} opened {} ({blocks} blocks{access})", self.name, crate::guid_text(unique));
    }

    /// Watch every session's doorbell, under `base` plus its id.
    pub fn watch(&self, poller: &Poller, base: u64) {
        for (id, s) in &self.sessions {
            if !s.closing {
                poller.watch(&s.heard, READABLE, base + id);
            }
        }
    }

    pub fn ids(&self) -> Vec<u64> {
        self.sessions.keys().copied().collect()
    }

    /// Put `c` on its session's completion ring. [`Self::pull`] leaves room for
    /// every answer, so a ring without it is a client whose head went back or
    /// past what was posted: the session ends.
    fn post(session: &mut Served, c: Completion) {
        match session.rings.1.push(session.region.words(), c.encode()) {
            Ok(true) => session.posted = true,
            Ok(false) | Err(_) => session.closing = true,
        }
    }

    /// Hand one device answer to the session it is for.
    pub fn answered(&mut self, owner: Owner, ok: bool, status: u32) {
        if owner.session == JOB {
            let job = self.job.as_mut().unwrap_or_else(|| panic!("{}: an answer for a designation that ended", self.name));
            if !ok {
                println!("{}: the designation's command failed with status {status:#x}", self.name);
            }
            return job.steps.answered(ok);
        }
        let s = self.sessions.get_mut(&owner.session).unwrap_or_else(|| {
            panic!("{}: an answer for session {}, which ended with requests on the device", self.name, owner.session)
        });
        if let Some(c) = s.state.complete(owner.tag, ok, &mut self.holds, self.losses) {
            if !ok {
                println!("{}: session {}: request {} failed with status {status:#x}", self.name, owner.session, owner.tag);
            }
            if c.status == Status::Lost {
                println!(
                    "{}: session {}: a flush found writes of its own the device lost since it acknowledged \
                     them; the client is told Lost",
                    self.name, owner.session
                );
            }
            Self::post(s, c);
        }
    }

    /// Take what each session's client has published, while the device and
    /// the session's completion ring have room for it.
    pub fn pull(&mut self, dev: &mut impl Device) {
        for (&id, s) in self.sessions.iter_mut() {
            loop {
                if s.closing {
                    break;
                }
                let page = s.region.words();
                // Room for every answer: what is on the device, and what is
                // posted and not yet read, never exceeds the completion ring.
                let Ok(space) = s.rings.1.space(page) else {
                    s.closing = true;
                    break;
                };
                let unread = (DEPTH - space) as usize;
                if s.state.inflight().len() + unread >= DEPTH as usize || !dev.has_room() {
                    break;
                }
                let words = match s.rings.0.pop(page) {
                    Ok(Some(words)) => words,
                    Ok(None) => break,
                    Err(_) => {
                        s.closing = true;
                        break;
                    }
                };
                s.requests += 1;
                match s.state.take(words) {
                    Taken::Answer(c) => Self::post(s, c),
                    Taken::Issue(req) => {
                        let write = matches!(req.op, Op::Write { .. });
                        let owner = Owner { session: id, tag: req.tag, write };
                        match req.op {
                            Op::Read { run, lba } | Op::Write { run, lba } => {
                                let at = s.device_addr + run.span().offset as u64;
                                dev.submit_io(write, s.state.first() + lba, run.count(), at, owner);
                            }
                            Op::Flush => {
                                if !dev.submit_flush(owner) {
                                    let c = s.state.complete(req.tag, true, &mut self.holds, self.losses);
                                    Self::post(s, c.expect("just taken"));
                                }
                            }
                        }
                    }
                }
            }
            s.rings.0.release(s.region.words());
        }
        self.work(dev);
    }

    /// Publish what each session was answered, and ring its doorbell.
    pub fn publish(&mut self) {
        for s in self.sessions.values_mut() {
            if !s.posted {
                continue;
            }
            s.posted = false;
            if s.rings.1.publish(s.region.words()) && s.ring.ring().is_err() {
                s.closing = true;
            }
        }
    }

    /// Take session `id`'s doorbell; once its client has left, the session is
    /// closing.
    pub fn hear(&mut self, id: u64) {
        let s = self.sessions.get_mut(&id).expect("a session");
        if s.heard.try_take().is_err() {
            s.closing = true;
        }
    }

    /// End every session that is closing and has nothing on the device: its
    /// region leaves the controller's domain, and its partition is free.
    /// Answers how many ended.
    pub fn retire(&mut self, dev: &mut impl Device) -> usize {
        let done: Vec<u64> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.closing && s.state.inflight().len() == 0)
            .map(|(id, _)| *id)
            .collect();
        let ended = done.len();
        for id in done {
            let s = self.sessions.remove(&id).expect("just listed");
            if let Err(why) = dev.dma_unmap(s.device_addr) {
                panic!("{}: the kernel would not take session {id}'s region back: {why:?}", self.name);
            }
            self.holds.release(s.state.first());
            println!("{}: session {id} on {} closed after {} requests", self.name, crate::guid_text(s.unique), s.requests);
        }
        ended
    }

    /// The device was reset under every command it held: each is answered
    /// not done, and the loss count goes up, since the reset may have
    /// dropped the device's cache. Answers how many were the sessions'.
    pub fn reset(&mut self) -> usize {
        self.losses += 1;
        if let Some(job) = &mut self.job {
            job.steps.reset();
        }
        let mut answered = 0;
        for s in self.sessions.values_mut() {
            for c in s.state.abort_all() {
                answered += 1;
                Self::post(s, c);
            }
        }
        answered
    }

    /// The disk left: every request on it is answered not done, every
    /// client told, and every session ended.
    pub fn end(&mut self, dev: &mut impl Device) {
        self.reset();
        self.work(dev);
        for s in self.sessions.values_mut() {
            s.closing = true;
        }
        self.publish();
        self.retire(dev);
    }

    pub fn losses(&self) -> u64 {
        self.losses
    }

    /// Describe the disk to `conn`: its head, then each used entry of the
    /// table in force, then the end. One `try_send` each; a client that will
    /// not take them is let go.
    pub fn describe(&self, conn: Connection) {
        let copies = self.read.copies.as_deref();
        let head = DiskHead {
            lba_bytes: self.read.lba_bytes,
            sectors: self.read.sectors,
            agree: copies.is_some_and(|c| c.agree()),
            primary: copies.map_or_else(|| "unread".into(), |c| table::state(&c.primary)),
            backup: copies.map_or_else(|| "unread".into(), |c| table::state(&c.backup)),
        };
        if conn.try_send_bytes(wire::MSG_DISK, &head.encode()).is_err() {
            return;
        }
        if let Some((_, in_force)) = copies.and_then(|c| c.in_force()) {
            for (index, slot) in in_force.used() {
                let span = self.parts.iter().find(|p| p.unique == slot.unique_guid.0).and_then(|p| p.span.as_ref().ok());
                let line = SlotLine {
                    index,
                    kind: slot.type_guid.0,
                    unique: slot.unique_guid.0,
                    first: slot.first,
                    last: slot.last,
                    attributes: slot.attributes,
                    name: slot.name,
                    held: span.is_some_and(|&(first, blocks)| self.holds.holder(first, first + blocks).is_some()),
                    served: span.is_some(),
                };
                if conn.try_send_bytes(wire::MSG_SLOT, &line.encode()).is_err() {
                    return;
                }
            }
        }
        let _ = conn.try_send_bytes(wire::MSG_DESCRIBED, &[]);
    }

    /// The span `order` designates, held for it; or its refusal, judged
    /// against this disk's own state in this turn: the running system's
    /// partitions, its table and its holds.
    fn admit(&mut self, order: &Designation, dev: &mut impl Device) -> Result<(u64, u64), Refusal> {
        if self.job.is_some() {
            return Err(Refusal::Exhausted);
        }
        if order.active {
            return Err(Refusal::Active);
        }
        self.place(order.unique, dev)
    }

    /// Begin the designation the service admitted, answering `conn` when it
    /// ends; or refuse it now ([`Self::admit`]).
    pub fn designate(&mut self, dev: &mut impl Device, conn: Connection, order: Designation) {
        let what = crate::guid_text(order.unique);
        let name = self.name.clone();
        let refuse = |conn: &Connection, why: Refusal| {
            println!("{name}: a designation of {what} refused: {why:?}");
            let _ = conn.try_send_bytes(wire::MSG_REFUSED, &why.encode());
        };
        let (first, blocks) = match self.admit(&order, dev) {
            Ok(span) => span,
            Err(why) => return refuse(&conn, why),
        };
        let region = Region::create().and_then(|region| dev.dma_map(region.handle()).map(|at| (region, at)));
        let (region, device_addr) = match region {
            Ok(mapped) => mapped,
            Err(why) => {
                self.holds.release(first);
                println!("{}: no region for a designation: {why:?}", self.name);
                return refuse(&conn, Refusal::Exhausted);
            }
        };
        println!("{}: designating {what} ({blocks} blocks){}", self.name, if order.commit { "" } else { ", judging only" });
        let steps = designate::Designation::new(blocks, order.commit);
        self.job = Some(Job { conn, order, steps, region, device_addr, first });
        self.work(dev);
    }

    /// Move the designation on as far as the device lets it.
    fn work(&mut self, dev: &mut impl Device) {
        while let Some(job) = &mut self.job {
            let owner = |write| Owner { session: JOB, tag: 0, write };
            match job.steps.next(&mut job.region, dev.has_room()) {
                Ask::Wait => return,
                Ask::Read { block, count, at } => dev.submit_io(false, job.first + block, count, job.device_addr + at as u64, owner(false)),
                Ask::Write { block, at } => dev.submit_io(true, job.first + block, 1, job.device_addr + at as u64, owner(true)),
                Ask::Flush => {
                    if !dev.submit_flush(owner(false)) {
                        job.steps.answered(true);
                    }
                }
                Ask::Done(outcome) => return self.finish(dev, outcome),
            }
        }
    }

    /// End the designation: its region out of the controller's domain, its
    /// partition free, and its client answered.
    fn finish(&mut self, dev: &mut impl Device, outcome: Result<Designated, Refusal>) {
        let job = self.job.take().expect("a designation");
        if let Err(why) = dev.dma_unmap(job.device_addr) {
            panic!("{}: the kernel would not take a designation's region back: {why:?}", self.name);
        }
        self.holds.release(job.first);
        let what = crate::guid_text(job.order.unique);
        match outcome {
            Ok(done) => {
                println!("{}: designation of {what}: {done:?}", self.name);
                let _ = job.conn.try_send_bytes(wire::MSG_DESIGNATED, &done.encode());
            }
            Err(why) => {
                println!("{}: designation of {what} failed: {why:?}", self.name);
                let _ = job.conn.try_send_bytes(wire::MSG_REFUSED, &why.encode());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No device: `place` reaches none of it but `retire`, which has nothing
    /// to retire.
    struct Nothing;

    impl Device for Nothing {
        fn has_room(&self) -> bool {
            unreachable!()
        }
        fn submit_io(&mut self, _: bool, _: u64, _: u32, _: u64, _: Owner) {
            unreachable!()
        }
        fn submit_flush(&mut self, _: Owner) -> bool {
            unreachable!()
        }
        fn dma_map(&mut self, _: toyos::BorrowedHandle<'_>) -> Result<u64, SyscallError> {
            unreachable!()
        }
        fn dma_unmap(&mut self, _: u64) -> Result<(), SyscallError> {
            unreachable!()
        }
    }

    fn disk() -> Sessions {
        let parts = vec![
            Partition { unique: [1; 16], kind: [9; 16], span: Ok((0, 100)) },
            Partition { unique: [2; 16], kind: [9; 16], span: Err("not whole blocks".into()) },
        ];
        Sessions::new("test".into(), Read { lba_bytes: 512, sectors: 1 << 20, copies: None, parts: Ok(parts) })
    }

    fn order(unique: [u8; 16], active: bool) -> Designation {
        Designation { disk: 0, unique, active, commit: true }
    }

    /// A designation is judged against the disk's own state before it takes
    /// anything: a partition the running system is on, one a session holds,
    /// one the table does not name and one it will not serve are each
    /// refused, and none of them holds the partition.
    #[test]
    fn a_designation_is_refused_on_the_disks_own_state_before_it_holds_anything() {
        let mut disk = disk();
        let refusals = [
            (order([1; 16], true), Refusal::Active),
            (order([3; 16], false), Refusal::NotFound),
            (order([2; 16], false), Refusal::Unusable),
        ];
        for (order, want) in refusals {
            assert_eq!(disk.admit(&order, &mut Nothing), Err(want), "{order:?}");
        }
        assert_eq!(disk.place([1; 16], &mut Nothing), Ok((0, 100)), "nothing refused held it");
        assert_eq!(disk.admit(&order([1; 16], false), &mut Nothing), Err(Refusal::Held));
    }

    /// What only the disk knows refuses an open the service admitted: a
    /// partition its table no longer names, one it will not serve, the zero
    /// GUID, and one a session holds.
    #[test]
    fn a_disk_refuses_what_its_own_table_and_holds_say() {
        let mut disk = disk();
        assert_eq!(disk.place([3; 16], &mut Nothing), Err(Refusal::NotFound));
        assert_eq!(disk.place([0; 16], &mut Nothing), Err(Refusal::NotFound));
        assert_eq!(disk.place([2; 16], &mut Nothing), Err(Refusal::Unusable));
        assert_eq!(disk.place([1; 16], &mut Nothing), Ok((0, 100)));
        assert_eq!(disk.place([1; 16], &mut Nothing), Err(Refusal::Held));
    }

    #[test]
    fn a_disk_offers_every_partition_its_table_names_and_which_it_serves() {
        assert_eq!(
            disk().offered(),
            [
                Part { unique: [1; 16], kind: [9; 16], blocks: Some(100) },
                Part { unique: [2; 16], kind: [9; 16], blocks: None },
            ]
        );
    }
}
