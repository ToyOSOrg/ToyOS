//! Hardware device access.
//!
//! Typed device wrappers, one per class. **None of them opens anything**: a
//! claim is minted by the supervisor alone and arrives in this process's endowment
//! table under `dev:<class>`, so [`crate::endow::device`] is the only way to
//! get one and a program the manifest gives no device cannot express reaching
//! hardware.

use toyos_abi::syscall::{self, SyscallError};
use crate::{Device, AsHandle};
use crate::ipc::IpcPayload;
use toyos_abi::RawHandle;

/// SAFETY: each is `#[repr(C)]` over unsigned integers, arrays of them and
/// `RawHandle`s (`repr(transparent)` over a `u32`), so none holds a pointer or a field with
/// an invalid bit pattern, and each is declared through `user_safe!`,
/// which refuses a byte outside a field.
macro_rules! device_info {
    ($($t:ty),* $(,)?) => { $(unsafe impl IpcPayload for $t {})* };
}
device_info!(
    toyos_abi::FramebufferInfo,
    toyos_abi::pci::PciFunctionInfo,
    toyos_abi::acpi::AcpiInfo,
);

/// Read a claim's description, whose buffer fields are handles this call
/// installs.
///
/// **They are installed once.** The kernel remembers what it minted for a
/// claim and answers a later read with the same numbers, so the description is
/// read once per claim and the handles in it are owned from then on — reading
/// twice and adopting both answers closes one buffer twice.
///
/// Bounded by [`IpcPayload`] and not by `Copy`: the answer's bytes are
/// *reinterpreted* as a `T`, so what the bound has to promise is that every bit
/// pattern of that width is one — the contract [`crate::ipc::recv_payload`]
/// reads on, held here by the kernel rather than by a peer.
pub(crate) fn read_info<T: IpcPayload>(dev: &Device) -> Result<T, SyscallError> {
    let size = core::mem::size_of::<T>();
    let mut val = core::mem::MaybeUninit::<T>::uninit();
    // SAFETY: the slice covers exactly the `T` being filled, and `IpcPayload`
    // promises every bit pattern the read can write is a valid `T`.
    let buf = unsafe {
        core::slice::from_raw_parts_mut(val.as_mut_ptr() as *mut u8, size)
    };
    let n = syscall::read(dev.0.0, buf)?;
    assert_eq!(n, size, "device info size mismatch");
    // SAFETY: the assertion above proves the read filled all `size` bytes.
    Ok(unsafe { val.assume_init() })
}

pub struct FramebufferDev(pub(crate) Device);

impl FramebufferDev {
    pub fn info(&self) -> Result<toyos_abi::FramebufferInfo, SyscallError> {
        read_info(&self.0)
    }
}

impl AsHandle for FramebufferDev {
    fn as_handle(&self) -> RawHandle { self.0.as_handle() }
}

/// One PCI function, driven by this process: the kernel keeps config space and
/// the interrupt vector, and the holder gets the register window, buffers the
/// function can reach and nothing else can, and the interrupt as records read
/// off this handle.
///
/// **A claim is read once as a description and afterwards as interrupts.**
/// [`Self::describe`] must be called exactly once, before [`Self::irq`]. The
/// kernel keeps the flag, so nothing here can enforce it.
pub struct PciDev(pub(crate) Device);

/// A DMA buffer, in this process and at the device.
pub struct DmaRegion {
    pub memory: crate::shm::SharedMemory,
    /// What a descriptor must carry for the device to reach `memory`. Never a
    /// physical address once a unit translates for this function.
    pub device_addr: u64,
}

impl PciDev {
    /// This function's description; see the type's note — exactly once.
    pub fn describe(&self) -> Result<toyos_abi::pci::PciFunctionInfo, SyscallError> {
        read_info(&self.0)
    }

    /// One dword, word or byte of this function's own config space.
    ///
    /// **Read-only, and there is no writing counterpart**: a driver cannot find
    /// its own registers without its capability chain, while every write config
    /// space takes — bus mastering, the BARs, the MSI-X control word — is a
    /// decision the kernel keeps.
    pub fn config_read(
        &self,
        offset: u32,
        width: toyos_abi::syscall::RegWidth,
    ) -> Result<u32, SyscallError> {
        syscall::device_reg_read(self.0.as_handle(), u64::from(offset), width)
    }

    /// Map memory BAR `bar`, bounded to the `bar_bytes` the description gave.
    ///
    /// The kernel's window is whole 4 KiB pages, so a BAR smaller than one is
    /// mapped with the rest of its page; the length here is the BAR's.
    pub fn map_bar(&self, bar: u32, bytes: u64) -> Result<crate::shm::SharedMemory, SyscallError> {
        let handle = syscall::device_bar_map(self.0.as_handle(), bar)?;
        crate::shm::SharedMemory::adopt(handle, bytes as usize)
    }

    /// Give up ownership, for a claim about to be endowed.
    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }

    /// State that this function issues `bits`-bit device addresses, before any
    /// grant or lend: every one is placed below `1 << bits`.
    pub fn dma_limit(&self, bits: u8) -> Result<(), SyscallError> {
        syscall::device_dma_limit(self.0.as_handle(), bits)
    }

    /// Let this function master the bus, once its driver has stopped it and
    /// laid out what it may reach. Until then it issues nothing, an interrupt
    /// included.
    pub fn dma_start(&self) -> Result<(), SyscallError> {
        syscall::device_dma_start(self.0.as_handle())
    }

    /// `bytes` of memory this function can reach and nothing else can, below
    /// the width [`Self::dma_limit`] stated.
    pub fn dma_alloc(&self, bytes: u64) -> Result<DmaRegion, SyscallError> {
        let grant = syscall::device_dma_alloc(self.0.as_handle(), bytes)?;
        let memory = crate::shm::SharedMemory::adopt(grant.shm, grant.bytes as usize)?;
        Ok(DmaRegion { memory, device_addr: grant.device_addr })
    }

    /// Put a region this process holds — typically one a client sent — into
    /// this function's address space, and answer where the function reaches
    /// it. The region stays alive while it is mapped; [`Self::dma_unmap`] or
    /// the claim's end takes it back.
    pub fn dma_map(&self, region: RawHandle) -> Result<toyos_abi::pci::DmaMapping, SyscallError> {
        syscall::device_dma_map(self.0.as_handle(), region)
    }

    /// Take back what [`Self::dma_map`] put at `device_addr`: from here the
    /// function reaches none of it.
    pub fn dma_unmap(&self, device_addr: u64) -> Result<(), SyscallError> {
        syscall::device_dma_unmap(self.0.as_handle(), device_addr)
    }

    /// The interrupts since the last read, or `Err(WouldBlock)` for none.
    pub fn irq(&self) -> Result<toyos_abi::pci::DeviceIrqRecord, SyscallError> {
        irq_record(&self.0)
    }
}

/// A claim's interrupts since its last read, or `Err(WouldBlock)` for none.
fn irq_record(dev: &Device) -> Result<toyos_abi::pci::DeviceIrqRecord, SyscallError> {
    let mut record = toyos_abi::pci::DeviceIrqRecord { count: 0, _pad: 0, first_nanos: 0, last_nanos: 0 };
    // SAFETY: the slice covers exactly the record being filled, and every
    // bit pattern of each of its integer fields is a valid one.
    let buf = unsafe {
        core::slice::from_raw_parts_mut(&mut record as *mut _ as *mut u8, toyos_abi::pci::DeviceIrqRecord::SIZE)
    };
    let n = syscall::read_nonblock(dev.0.0, buf)?;
    assert_eq!(n, toyos_abi::pci::DeviceIrqRecord::SIZE, "partial device interrupt record ({n} bytes)");
    Ok(record)
}

/// The machine's ACPI fixed hardware, served by this process
/// ([`toyos_abi::acpi`]): its event blocks as ports,
/// and its SCI as records.
///
/// **Read once as a description, which binds the ports to this process, and
/// afterwards as interrupts**, as a [`PciDev`] is; the SCI is masked from the
/// claim, and after every interrupt, until [`Self::ack`].
pub struct AcpiDev(pub(crate) Device);

impl AcpiDev {
    pub fn describe(&self) -> Result<toyos_abi::acpi::AcpiInfo, SyscallError> {
        read_info(&self.0)
    }

    pub fn irq(&self) -> Result<toyos_abi::pci::DeviceIrqRecord, SyscallError> {
        irq_record(&self.0)
    }

    /// Every status bit behind the SCI is served: unmask it.
    pub fn ack(&self) -> Result<(), SyscallError> {
        syscall::write(self.0.0.0, &toyos_abi::acpi::ACK.to_ne_bytes()).map(|_| ())
    }

    /// Have the kernel make one access outside the claim's own ports, or
    /// refuse it by name; beside either, the UEFI type firmware's map gives a
    /// memory address ([`toyos_abi::acpi::Access::memory_type`]).
    pub fn access(&self, mut access: toyos_abi::acpi::Access) -> Result<(Result<u64, toyos_abi::acpi::Refused>, u8), SyscallError> {
        let made = syscall::acpi_access(self.as_handle(), &mut access)?;
        Ok((made, access.memory_type))
    }

    /// Try the firmware's Global Lock ([`syscall::acpi_lock_take`]): `false`
    /// where the firmware owns it and will raise `GBL_STS` on letting go.
    pub fn lock_take(&self) -> Result<bool, SyscallError> {
        syscall::acpi_lock_take(self.as_handle())
    }

    pub fn lock_release(&self) -> Result<(), SyscallError> {
        syscall::acpi_lock_release(self.as_handle())
    }

    /// Supply `\_S5`'s `SLP_TYPa` for the kernel's power-off
    /// ([`syscall::acpi_s5`]).
    pub fn s5(&self, slp_typ_a: u64) -> Result<(), SyscallError> {
        syscall::acpi_s5(self.as_handle(), slp_typ_a)
    }

    /// Declare one processor's idle states deeper than C1
    /// ([`syscall::acpi_idle`]).
    pub fn idle(&self, declaration: &toyos_abi::acpi::IdleDeclaration) -> Result<(), SyscallError> {
        syscall::acpi_idle(self.as_handle(), declaration)
    }
}

impl AsHandle for AcpiDev {
    fn as_handle(&self) -> RawHandle { self.0.as_handle() }
}

impl AsHandle for PciDev {
    fn as_handle(&self) -> RawHandle { self.0.as_handle() }
}

