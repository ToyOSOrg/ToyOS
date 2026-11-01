//! The machine's controller: its two ports, opened to this process by its
//! `isa` claim, and the claim's interrupt records.

use toyos::poller::{Poller, READABLE};
use toyos::{AsHandle, Device};
use toyos_abi::ioport;
use toyos_abi::pci::DeviceIrqRecord;
use toyos_abi::syscall::{self, SyscallError};

use crate::bus::{Controller, Irq, Records};

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;

pub struct Hw {
    claim: Device,
    poller: Poller,
    /// A watch on the claim is registered and has not fired.
    watching: bool,
}

impl Hw {
    /// Over `claim`, whose description is read here: that read binds the
    /// row's ports to this process.
    pub fn bind(claim: Device) -> Self {
        let mut description = [0u8; 16];
        let n = claim.read(&mut description).expect("ps2server: the claim's first read is its description");
        assert_eq!(n, description.len(), "ps2server: a description of {n} bytes");
        Self { claim, poller: Poller::new(1), watching: false }
    }
}

impl Records for Hw {
    fn clock(&mut self) -> u64 {
        toyos_abi::clock::nanos_since_boot()
    }

    /// The claim's record, if it holds one.
    fn take(&mut self) -> Option<Irq> {
        let mut record = [0u8; DeviceIrqRecord::SIZE];
        match syscall::read_nonblock(self.claim.as_handle().raw(), &mut record) {
            Ok(n) => {
                assert_eq!(n, DeviceIrqRecord::SIZE, "ps2server: a record of {n} bytes");
                let first = record[8..16].try_into().expect("a record's first time is eight bytes");
                Some(Irq { first_nanos: u64::from_ne_bytes(first) })
            }
            Err(SyscallError::WouldBlock) => None,
            Err(other) => panic!("ps2server: a bound claim's read answered {other:?}"),
        }
    }

    fn park(&mut self, now: u64, deadline: u64) {
        if !self.watching {
            self.poller.watch(&self.claim, READABLE, 0);
            self.watching = true;
        }
        let timeout = if deadline == u64::MAX { u64::MAX } else { deadline - now };
        let mut fired = false;
        self.poller.wait(1, timeout, |_| fired = true);
        if fired {
            self.watching = false;
        }
    }
}

impl Controller for Hw {
    fn status(&mut self) -> u8 {
        ioport::in8(STATUS)
    }

    fn data(&mut self) -> u8 {
        ioport::in8(DATA)
    }

    fn command(&mut self, byte: u8) {
        ioport::out8(STATUS, byte);
    }

    fn write(&mut self, byte: u8) {
        ioport::out8(DATA, byte);
    }

    fn now(&mut self) -> u64 {
        self.clock()
    }

    fn wait(&mut self, deadline: u64) -> Option<Irq> {
        self.first(deadline)
    }
}
