//! A device claim, and the one console every kernel-spawned process starts on.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::SyscallError;
use toyos_abi::usersafe::bytes;
use toyos_abi::FramebufferInfo;

use crate::device::{Claim, DeviceType};
use crate::mm::charge::Exhausted;

use super::handle::{HandleEntry, HandleTable};
use super::shm::SharedMemObject;
use super::{Held, KObjectRef, KObjectVariant, ObjectCore, ZeroHandles};

/// What the class answers when its holder reads it.
pub enum DeviceInfo {
    /// The scanout, whose handle the mint installs.
    Framebuffer(FramebufferInfo, Arc<SharedMemObject>),
    /// The function. No buffer handle beside it: a PCI claimant asks for its
    /// own memory, so this mint installs nothing.
    PciFunction(toyos_abi::pci::PciFunctionInfo),
    /// The ports and lines granted, as the selector named them.
    Isa(toyos_abi::syscall::IsaId),
    /// The fixed hardware's blocks.
    Acpi(toyos_abi::acpi::AcpiInfo),
}

// MAP and WRITE draw into it; DUP and TRANSFER let a daemon hand the buffer to another process.
const BUFFER_RIGHTS: Rights = Rights::MAP.union(Rights::WRITE).union(Rights::DUP).union(Rights::TRANSFER);

fn install_buffer(table: &mut HandleTable, buffer: &Arc<SharedMemObject>) -> Result<RawHandle, SyscallError> {
    table
        .install(HandleEntry::new(KObjectRef::SharedMem(Arc::clone(buffer)), BUFFER_RIGHTS))
        .map_err(|_| SyscallError::ResourceExhausted)
}

impl DeviceInfo {
    /// The description as bytes, with a handle installed for the one buffer it
    /// names. A table with no room leaves `described.bytes` unset, so the next
    /// read mints again.
    fn mint(&self, table: &mut HandleTable) -> Result<Box<[u8]>, SyscallError> {
        Ok(match self {
            Self::Framebuffer(info, scanout) => {
                let mut info = *info;
                info.scanout = install_buffer(table, scanout)?;
                bytes(&info).into()
            }
            // Nothing to install: every address in it is a size, and the memory
            // is what `SYS_DEVICE_DMA_ALLOC` answers later.
            Self::PciFunction(info) => bytes(info).into(),
            Self::Isa(set) => set.wire().iter().flat_map(|word| word.to_ne_bytes()).collect(),
            Self::Acpi(info) => bytes(info).into(),
        })
    }
}

/// One process's exclusive hold on a device.
pub struct DeviceClaim {
    pub(super) core: ObjectCore,
    class: DeviceType,
    // No Rights::DUP: at most one handle exists, so info_read needs no per-handle state.
    info_read: AtomicBool,
    described: crate::sync::Lock<Described>,
    reference: Held<Claim>,
}

// Minted once: re-minting on a second read would leak a handle each time.
struct Described {
    info: DeviceInfo,
    bytes: Option<Box<[u8]>>,
}

impl DeviceClaim {
    /// Refused, `claim` drops and the device is let go.
    pub fn new(class: DeviceType, info: DeviceInfo, claim: Claim) -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self {
            core: Self::new_core(0)?,
            class,
            info_read: AtomicBool::new(false),
            described: crate::sync::Lock::new(Described { info, bytes: None }),
            reference: Held::new(claim),
        }))
    }

    pub fn class(&self) -> DeviceType {
        self.class
    }

    /// `f` over the function this claim drives, with what `pcidev`'s calls
    /// take: `None` for a claim on anything else, and once the last handle has
    /// let the function go.
    ///
    /// Every call the substrate answers goes through this, under the lock the
    /// release takes the binding with: `f` has returned before the function is
    /// let go, or never runs. So `f` does not wait.
    pub fn pci<R>(&self, f: impl FnOnce(&crate::pcidev::Binding) -> R) -> Option<R> {
        self.reference.with(|claim| claim.binding().map(f)).flatten()
    }

    /// [`pci`](Self::pci), for the `isa` row of a claim on an ISA function or
    /// the ACPI fixed hardware.
    pub fn isa<R>(&self, f: impl FnOnce(&crate::isa::Row) -> R) -> Option<R> {
        self.reference.with(|claim| claim.row().map(f)).flatten()
    }

    /// Register `entry` on the watch of the function or row this claim holds,
    /// with what the claim lends, so before its release: the release answers
    /// every poll registered before it, and one that finds the claim released
    /// is ended here, by its registrant. No poll is left for the interrupts of
    /// whoever holds the slot or the row next.
    pub(crate) fn add_poll(&self, entry: crate::inbox::PollEntry) {
        let mut entry = Some(entry);
        self.reference.with(|claim| claim.add_poll(entry.take().expect("lent once")));
        if let Some(entry) = entry {
            kernel::sched::watch::Ring::fire(&entry, kernel::sched::watch::Fire::Gone);
        }
    }

    pub fn info_read(&self) -> bool {
        self.info_read.load(Ordering::Relaxed)
    }

    /// Write this claim's description into `buf`, minting handles on first call only.
    pub fn describe(
        &self,
        table: &mut HandleTable,
        buf: &mut crate::user_ptr::UserBytesMut,
    ) -> u64 {
        let mut described = self.described.lock();
        if described.bytes.is_none() {
            match described.info.mint(table) {
                Ok(minted) => described.bytes = Some(minted),
                Err(e) => return e.to_u64(),
            }
        }
        let bytes = described.bytes.as_deref().expect("just minted");
        let count = buf.len().min(bytes.len());
        buf.write_at(0, &bytes[..count]);
        self.info_read.store(true, Ordering::Relaxed);
        count as u64
    }
}

impl DeviceClaim {
    /// Every mapping made through this claim taken back
    /// (`pcidev::revoke_handed`): at its end, and when it moves to another
    /// holder, so its last holder keeps none.
    pub fn revoke_mappings(&self) {
        self.pci(crate::pcidev::revoke_handed);
    }
}

// Released on last handle, not last Arc: a parked daemon can strand an Arc without releasing.
impl ZeroHandles for DeviceClaim {
    fn on_zero_handles(&self) {
        self.revoke_mappings();
        self.reference.release();
    }
}

/// One holder's view of the machine's serial console — each holder gets its own, never a shared handle.
pub struct ConsoleObject {
    pub(super) core: ObjectCore,
    // Lock order process_data -> line -> the console queue; taking them in reverse deadlocks.
    line: crate::sync::Lock<crate::drivers::serial::ConsoleLine>,
}

impl ConsoleObject {
    /// A console of its own for a holder in `job`, charged to it.
    pub fn new(job: &Arc<kernel::sched::job::Job>) -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self {
            core: Self::new_core_for(job, 0)?,
            line: crate::sync::Lock::new(crate::drivers::serial::ConsoleLine::new()),
        }))
    }

    /// Take a userland write, queueing every whole line it completes that the
    /// console has room for; answers the bytes taken.
    pub fn write(&self, buf: &crate::user_ptr::UserBytes) -> usize {
        self.line.lock().write(buf)
    }
}

// Flushes the partial line on last handle instead of dropping it.
impl Drop for ConsoleObject {
    fn drop(&mut self) {
        self.line.lock().finish();
    }
}
