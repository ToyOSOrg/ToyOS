//! A region a client lent, as the node's [`Session`]: its header as words, its two rings as
//! [tcp]'s buffers, and the stream's [`Holders`], every one of whose two bells a ring reaches.
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
use toyos::net::stream::{Words, RECEIVE_BYTES, RECEIVE_RING, REGION_BYTES, SEND_BYTES, SEND_RING};
use toyos::shm::{self, SharedMemory};
use toyos::transport::session::{Holder, Rings};
use toyos::wake::{Bell, Waker};
use toyos::{Connection, OwnedHandle};
use toyos_abi::syscall::SyscallError;
use toyos_net_node::Session;
use toyos_net_tcp::Lent;

/// The region's mapping.
struct Mapping(SharedMemory);

/// One of the region's rings, as [tcp]'s storage.
struct Ring {
    mapping: Rc<Mapping>,
    base: usize,
    len: usize,
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ring({:#x}, {})", self.base, self.len)
    }
}

impl Ring {
    /// The address of `len` bytes at `at`, which [`Lent`]'s contract keeps inside the ring.
    fn at(&self, at: usize, len: usize) -> *mut u8 {
        assert!(at.checked_add(len).is_some_and(|end| end <= self.len), "{len} bytes at {at} run past a ring of {}", self.len);
        // SAFETY: the ring lies inside the mapping, which `mapping` keeps alive, and the
        // assertion keeps the bytes inside the ring.
        unsafe { self.mapping.0.as_ptr().add(self.base + at) }
    }
}

impl Lent for Ring {
    fn len(&self) -> usize {
        self.len
    }

    fn put(&mut self, at: usize, data: &[u8]) {
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

    fn receive(&self) -> Box<dyn Lent> {
        Box::new(Ring { mapping: self.mapping.clone(), base: RECEIVE_RING, len: RECEIVE_BYTES as usize })
    }

    fn send(&self) -> Box<dyn Lent> {
        Box::new(Ring { mapping: self.mapping.clone(), base: SEND_RING, len: SEND_BYTES as usize })
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
