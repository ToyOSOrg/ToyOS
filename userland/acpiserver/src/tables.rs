//! The machine's tables, fetched through the kernel into this process:
//! [`Tables`] is the physical memory [`toyos_acpi`] decodes from, read one
//! mediated access at a time ([`crate::host::Kernel`]) and kept, so a table
//! is checked and loaded from bytes this server holds.
//!
//! [`Phys::readable`] is where a range is fetched, whole or not at all, and
//! [`Phys::byte`] reads what was fetched: the decoder asks for every range
//! before it reads one, which is the trait's contract. A range the kernel
//! refuses is not readable, and why is kept by its address for whoever logs
//! the table ([`Tables::refused`]). Address zero is no table's.

use std::cell::{Cell, RefCell};

use toyos_abi::acpi::{Space, Width};
use toyos_acpi::Phys;

use crate::host::{self, Kernel, Pages, Refusal};
use crate::ledger::Ledger;

pub struct Tables<'k, K> {
    kernel: &'k K,
    /// Every range fetched, at its address.
    held: RefCell<Vec<(u64, Vec<u8>)>>,
    pub reads: Cell<u64>,
    pub pages: RefCell<Pages>,
    /// Each range that was not readable, by where it begins, and why.
    refused: RefCell<Vec<(u64, Refusal)>>,
    /// The kernel answered that the machine is stopping.
    pub stopping: Cell<bool>,
}

impl<'k, K: Kernel> Tables<'k, K> {
    pub fn new(kernel: &'k K) -> Self {
        Tables {
            kernel,
            held: RefCell::new(Vec::new()),
            reads: Cell::new(0),
            pages: RefCell::new(Pages::default()),
            refused: RefCell::new(Vec::new()),
            stopping: Cell::new(false),
        }
    }

    /// Why the range that begins at `phys` was not readable, the last time
    /// it was asked for.
    pub fn refused(&self, phys: u64) -> Option<Refusal> {
        self.refused.borrow().iter().rev().find(|(at, _)| *at == phys).map(|&(_, why)| why)
    }

    /// Every refusal of a range, counted by what it says.
    pub fn refusals(&self) -> Ledger {
        let mut ledger = Ledger::default();
        for (_, why) in self.refused.borrow().iter() {
            ledger.see(&why.to_string());
        }
        ledger
    }

    /// Why the last range that was not readable was not.
    pub fn last_refused(&self) -> Option<Refusal> {
        self.refused.borrow().last().map(|&(_, why)| why)
    }

    /// `len` bytes at `phys`, in qwords and then in bytes, so no read reaches
    /// past the range asked for into memory of another type.
    fn fetch(&self, phys: u64, len: usize) -> Result<Vec<u8>, Refusal> {
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            let width = if len - bytes.len() >= 8 { Width::QWord } else { Width::Byte };
            let at = phys + bytes.len() as u64;
            let (value, memory_type) = host::read(self.kernel, Space::SystemMemory, at, width)?;
            self.reads.set(self.reads.get() + 1);
            self.pages.borrow_mut().read(at, memory_type);
            bytes.extend_from_slice(&value.to_le_bytes()[..width.bytes() as usize]);
        }
        Ok(bytes)
    }
}

impl<K: Kernel> Phys for &Tables<'_, K> {
    fn readable(self, phys: u64, len: usize) -> bool {
        let Some(end) = phys.checked_add(len as u64) else { return false };
        if phys == 0 {
            return false;
        }
        if self.held.borrow().iter().any(|(base, bytes)| *base <= phys && end <= base + bytes.len() as u64) {
            return true;
        }
        match self.fetch(phys, len) {
            Ok(bytes) => {
                self.held.borrow_mut().push((phys, bytes));
                true
            }
            Err(refusal) => {
                self.stopping.set(self.stopping.get() || refusal == Refusal::Stopping);
                self.refused.borrow_mut().push((phys, refusal));
                false
            }
        }
    }

    fn byte(self, phys: u64) -> u8 {
        let held = self.held.borrow();
        // The newest first: a table's whole range is fetched after its header's.
        let byte = held.iter().rev().find_map(|(base, bytes)| phys.checked_sub(*base).and_then(|at| bytes.get(at as usize)));
        *byte.expect("acpiserver: the table decoder read a byte it never asked for, against `toyos_acpi::Phys`'s contract")
    }
}

#[cfg(test)]
mod tests {
    use toyos_abi::acpi::{Access, Refused};

    use super::*;
    use crate::host::tests::Scripted;

    const AT: u64 = 0x7fb0_0000;
    const KEPT: u64 = 0x7000_0000;

    fn machine() -> Scripted {
        Scripted { memory: vec![(AT, 9, (0..=40u8).collect())], kept: vec![(KEPT, KEPT + 0x1000, 5)], ..Default::default() }
    }

    #[test]
    fn a_range_is_fetched_whole_in_qwords_and_then_bytes_and_kept() {
        let kernel = machine();
        let tables = &Tables::new(&kernel);
        assert!(tables.readable(AT + 1, 19));
        let read = |at, width| Access::read(Space::SystemMemory, at, width);
        assert_eq!(
            *kernel.asked.borrow(),
            [read(AT + 1, Width::QWord), read(AT + 9, Width::QWord), read(AT + 17, Width::Byte), read(AT + 18, Width::Byte), read(AT + 19, Width::Byte)],
            "a qword that would have reached past the range was asked for"
        );
        assert_eq!((0..19).map(|i| tables.byte(AT + 1 + i)).collect::<Vec<u8>>(), (1..=19u8).collect::<Vec<u8>>());
        assert_eq!(tables.reads.get(), 5);
        assert_eq!(tables.pages.borrow().by_type(), "1 of type 9");

        // What is held is not fetched again; what reaches past it is.
        assert!(tables.readable(AT + 4, 8));
        assert_eq!(kernel.asked.borrow().len(), 5);
        assert!(tables.readable(AT + 4, 20));
        assert_eq!(kernel.asked.borrow().len(), 5 + 2 + 4);
        assert_eq!(tables.byte(AT + 23), 23);
        assert_eq!(tables.last_refused(), None);
    }

    #[test]
    fn a_range_the_kernel_refuses_any_byte_of_is_not_readable_and_says_why() {
        let kernel = machine();
        let tables = &Tables::new(&kernel);
        // The last byte is past what the firmware holds here.
        assert!(!tables.readable(AT + 32, 10));
        let ram = Refusal::Kernel { space: Space::SystemMemory, refused: Refused::UsableMemory, memory_type: 7 };
        assert_eq!(tables.refused(AT + 32), Some(ram));
        assert!(tables.readable(AT + 32, 9));
        // Memory of a type the kernel passes no read of, refused under that
        // name and type, each range by its own address.
        assert!(!tables.readable(KEPT + 8, 36));
        let kept = Refusal::Kernel { space: Space::SystemMemory, refused: Refused::MemoryType, memory_type: 5 };
        assert_eq!((tables.refused(KEPT + 8), tables.refused(AT + 32), tables.refused(AT)), (Some(kept), Some(ram), None));
        assert_eq!(tables.last_refused(), Some(kept));
        assert_eq!(tables.refusals().counts(), format!("{kept} x1; {ram} x1"));
        assert!(!tables.stopping.get());

        // Address zero and a range that wraps are no table's, and the kernel is not asked.
        let asked = kernel.asked.borrow().len();
        assert!(!tables.readable(0, 36));
        assert!(!tables.readable(u64::MAX - 3, 36));
        assert_eq!(kernel.asked.borrow().len(), asked);
    }

    #[test]
    fn a_stopping_machine_reads_no_table() {
        let kernel = machine();
        kernel.stops_after.set(Some(2));
        let tables = &Tables::new(&kernel);
        assert!(!tables.readable(AT, 36));
        assert!(tables.stopping.get());
        assert_eq!(tables.refused(AT), Some(Refusal::Stopping));
    }
}
