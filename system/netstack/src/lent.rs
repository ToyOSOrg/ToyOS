//! A region a client lent, as the node's [`Session`]: its header as words, its bytes as the
//! node's [`Memory`], and the stream's [`Holders`], every one of whose two bells a ring reaches.
//!
//! **A region is judged before it is mapped** ([`SharedMemory::lent`]): a handle that is no
//! region, one that is not memory the kernel allocated, and one that is not exactly
//! [`REGION_BYTES`] are refused by name, so netstack never maps a device's registers or a
//! region shorter than the layout.
//!
//! **The client writes the same memory**, so bytes are copied in and out once and no reference
//! is formed over it, as `toyos::fs`'s window is: a byte the client rewrites under a copy costs
//! only its own stream. The header is reached only as atomics.
//!
//! The mapping lives as long as anything holds it: the session, and [tcp]'s buffers, which
//! outlive the session where [tcp] finishes a connection its holders left. The holders live as
//! long as the session: dropped, every holder's connection and bells hang up.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use toyos::ipc::FrameRx;
use toyos::net::stream::{Words, REGION_BYTES};
use toyos::shm::{self, SharedMemory};
use toyos::transport::session::{Holder, Rings};
use toyos::wake::{Bell, Waker};
use toyos::{Connection, OwnedHandle};
use toyos_abi::syscall::SyscallError;
use toyos_net_node::{Memory, Session};

/// The region's mapping, as the node lays its rings out in it: a page is touched only once
/// [`Memory::commit`] made it memory, since a region is made with only its first rings'.
struct Mapping(SharedMemory);

impl std::fmt::Debug for Mapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mapping")
    }
}

impl Mapping {
    /// The address of `len` bytes at `at`, inside the region.
    fn at(&self, at: usize, len: usize) -> *mut u8 {
        assert!(at.checked_add(len).is_some_and(|end| end <= REGION_BYTES), "{len} bytes at {at} run past the region");
        // SAFETY: the assertion keeps the bytes inside the mapping, which `self` keeps alive.
        unsafe { self.0.as_ptr().add(at) }
    }
}

impl Memory for Mapping {
    fn put(&self, at: usize, data: &[u8]) {
        let to = self.at(at, data.len());
        // SAFETY: `at` bounded the bytes inside the mapping; `data` is netstack's own memory
        // and never the region.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), to, data.len()) }
    }

    fn take(&self, at: usize, out: &mut [u8]) {
        let from = self.at(at, out.len());
        // SAFETY: as in `put`, with the two sides swapped.
        unsafe { core::ptr::copy_nonoverlapping(from, out.as_mut_ptr(), out.len()) }
    }

    fn commit(&self, at: usize, len: usize) -> bool {
        self.0.commit(at, len).is_ok()
    }
}

/// One holder of a stream, as netstack keeps it.
pub struct Holding {
    /// What it asks on.
    pub connection: Connection,
    /// Its frames, each a bare export.
    pub frames: FrameRx<0>,
    /// The wakers of the two bells the holder holds, rung for a reader and for a writer.
    pub receive: Waker,
    pub send: Waker,
    /// The bell the holder rings, whose wakers going is its leaving.
    pub rung: Bell,
}

/// A stream's holders by number, and a handle of its region for the holders netstack makes.
pub struct Holders {
    region: OwnedHandle,
    pub by_number: RefCell<BTreeMap<Holder, Holding>>,
}

impl Holders {
    /// A second handle of the region, for a holder's [`ADOPT`](toyos::net::stream::ADOPT).
    pub fn region(&self) -> Result<OwnedHandle, SyscallError> {
        self.region.try_clone()
    }
}

/// A stream's session as netstack holds it.
pub struct Region {
    mapping: Rc<Mapping>,
    holders: Rc<Holders>,
}

impl Region {
    /// The region `handle` names, judged and mapped. Refused, nothing is mapped and the handle
    /// is closed.
    pub fn judged(handle: toyos::OwnedHandle) -> Result<SharedMemory, shm::Lent> {
        SharedMemory::lent(handle, REGION_BYTES)
    }

    /// The session over `mapping`, `first` its first holder, and its holders, which `serve`
    /// adds to and takes from as long as the session lives.
    pub fn new(mapping: SharedMemory, first: Holding) -> Result<(Self, Rc<Holders>), SyscallError> {
        let region = mapping.share()?;
        let holders = Rc::new(Holders { region, by_number: RefCell::new(BTreeMap::from([(Holder::FIRST, first)])) });
        Ok((Self { mapping: Rc::new(Mapping(mapping)), holders: holders.clone() }, holders))
    }
}

impl Session for Region {
    fn words(&self) -> &Words {
        // SAFETY: the mapping is `REGION_BYTES` long, 2 MiB aligned, and lives as long as
        // `self`; its header is `Words` and reached only as atomics.
        unsafe { &*(self.mapping.0.as_ptr() as *const Words) }
    }

    fn memory(&self) -> Rc<dyn Memory> {
        self.mapping.clone()
    }

    /// A ring into a bell its holder no longer holds is dropped: that
    /// holder's leaving is its rung bell's end.
    fn ring(&self, rings: Rings) {
        for holding in self.holders.by_number.borrow().values() {
            if rings.receive {
                holding.receive.wake();
            }
            if rings.send {
                holding.send.wake();
            }
        }
    }
}
