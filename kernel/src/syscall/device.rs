//! Syscalls that drive a claimed device.

use alloc::sync::Arc;

use crate::object::device::DeviceClaim;
use crate::object::{ops, KObjectRef};
use crate::user_ptr::SyscallContext;
use crate::UserAddr;
use crate::{device, process};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;

use super::handles::{demand_syscap, handle_result};

/// Reads one register of a claimed PCI function's own config space, through
/// the claim itself: what names the function is lent by it for the access.
///
/// **There is no writing counterpart, and that is the point**: a driver cannot
/// find its own registers without its capability chain — virtio's four config
/// windows are named by vendor capabilities and nothing else — while every
/// write config space takes is a decision this kernel keeps. Bus mastering,
/// the BARs and the MSI and MSI-X control words are all writes, and the absence
/// of a path for them is what stops a holder aiming its device at memory or
/// its interrupt at a vector.
pub(super) fn sys_device_reg_read(handle: RawHandle, offset: u64, width: u64) -> u64 {
    let Some(width) = toyos_abi::syscall::RegWidth::from_raw(width) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    let claim = process::with_process_data(|data| {
        data.handles.get::<DeviceClaim>(handle, Rights::NONE).map(|claim| {
            (claim.class() == device::DeviceType::PciFunction).then_some(claim)
        })
    });
    // `refuse` must run with nothing held; the guard above has already been dropped.
    let claim = match claim {
        Ok(claim) => claim,
        Err(e) => return e.refuse(),
    };
    // A claim with no registers to read answers `NotSupported`, distinct from no such device.
    let Some(claim) = claim else {
        return SyscallError::NotSupported.to_u64();
    };
    // The one place a caller's offset becomes an access: the witness
    // `pcidev::config_window` answers is the only thing the register read
    // takes, so an unchecked number cannot reach the register file.
    let read = match crate::pcidev::config_window(offset, width) {
        Ok(at) => on_function(&claim, |function| crate::pcidev::config_read(function, at, width)),
        Err(_) => Err(SyscallError::InvalidArgument),
    };
    match read {
        Ok(v) => v as u64,
        Err(e) => e.to_u64(),
    }
}

/// One operation on the `acpi` claim (`toyos_abi::acpi::op`), for the process
/// the claim's first read bound: a handle moved on to another answers
/// `PermissionDenied`, as its ports answer nothing there.
pub(super) fn sys_acpi(ctx: &SyscallContext, handle: RawHandle, op: u64, at: u64) -> u64 {
    use toyos_abi::acpi::op as ops;
    let claim = match process::with_process_data(|data| data.handles.get::<DeviceClaim>(handle, Rights::WRITE)) {
        Ok(claim) if claim.class() == device::DeviceType::Acpi => claim,
        Ok(claim) => {
            let held = claim.class().class_name();
            // `refuse` does not return: the claim is let go first.
            drop(claim);
            return crate::object::HandleError::WrongType { held, wanted: "an acpi claim" }.refuse();
        }
        Err(e) => return e.refuse(),
    };
    // Copied in before the row is borrowed and out after it is given back:
    // no user copy is made under the claim's lock.
    let mut request = match op {
        ops::ACCESS => match ctx.copy_in::<toyos_abi::acpi::Access>(UserAddr::new(at)) {
            Ok(request) => Some(request),
            Err(e) => return e.to_u64(),
        },
        _ => None,
    };
    let declaration = match op {
        ops::IDLE => match ctx.copy_in::<toyos_abi::acpi::IdleDeclaration>(UserAddr::new(at)) {
            Ok(declaration) => Some(declaration),
            Err(e) => return e.to_u64(),
        },
        _ => None,
    };
    let pid = process::current_process();
    // The holder's check and the act are one borrow of the row, as a read's are.
    let done = claim.isa(|row| {
        if !crate::isa::held_by(row, pid) {
            return Err(SyscallError::PermissionDenied);
        }
        match (op, request.as_mut()) {
            (ops::ACCESS, Some(request)) => crate::arch::acpi_mode::access(row, request).map(|()| 0),
            (ops::LOCK_TAKE, _) => {
                crate::arch::acpi_mode::lock_take(row).map(|taken| if taken { ops::TAKEN } else { ops::PENDING })
            }
            (ops::LOCK_RELEASE, _) => crate::arch::acpi_mode::lock_release(row).map(|()| 0),
            (ops::S5, _) => crate::arch::acpi_mode::s5(row, at).map(|()| 0),
            (ops::IDLE, _) => match &declaration {
                Some(declaration) => crate::arch::acpi_mode::idle(row, declaration).map(|()| 0),
                None => unreachable!("the declaration is copied in for IDLE"),
            },
            _ => Err(SyscallError::InvalidArgument),
        }
    });
    let done = done.unwrap_or(Err(SyscallError::Gone)).and_then(|word| match &request {
        Some(request) => ctx.copy_out(UserAddr::new(at), request).map(|()| word),
        None => Ok(word),
    });
    done.unwrap_or_else(|e| e.to_u64())
}

/// Mints a device claim, gated on a `SysCap` carrying [`Rights::DEVICE`].
///
/// `selector` says which device where the class alone does not — a PCI
/// function's arrival, an ISA set — and a word the class
/// does not read is refused, never dropped: whoever set it meant a device this
/// claim would not name.
pub(super) fn sys_device_claim(syscap: RawHandle, class: u64, selector: [u64; 2]) -> u64 {
    let Some(class) = device::DeviceType::from_raw(class) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    // A PCI function's second word is the firmware windows its claim is
    // granted at the mint (`toyos_abi::pci::FirmwareWindows`).
    let read = match class {
        device::DeviceType::Framebuffer | device::DeviceType::Acpi => 0,
        device::DeviceType::PciFunction => 2,
        device::DeviceType::Isa => 2,
    };
    if selector[read..].iter().any(|&word| word != 0) {
        return SyscallError::InvalidArgument.to_u64();
    }
    if let Err(e) = demand_syscap(syscap, Rights::DEVICE) {
        return e.refuse();
    }
    // Each refusal keeps its own word: the supervisor logs what it could not mint, and
    // "this machine has none" is a configuration while the rest are faults.
    let claim = match device::try_claim(class, selector) {
        Ok(c) => c,
        Err(device::ClaimError::Absent) => return SyscallError::NotFound.to_u64(),
        Err(device::ClaimError::Owned) => return SyscallError::AlreadyExists.to_u64(),
        Err(device::ClaimError::KernelDriven) => {
            return SyscallError::PermissionDenied.to_u64()
        }
        Err(device::ClaimError::Exhausted) => return SyscallError::ResourceExhausted.to_u64(),
        Err(device::ClaimError::Unusable) => return SyscallError::NotSupported.to_u64(),
    };
    process::with_process_data(|data| {
        handle_result(ops::install(&mut data.handles, KObjectRef::Device(claim)))
    })
}

/// The PCI function claim a handle names, with the right every substrate call
/// demands.
///
/// [`Rights::WRITE`] for all three: each one gives the holder control of the
/// device — a register window, memory the device can reach, its interrupt — and
/// a claim carries no `DUP`, so no narrower handle to one can exist.
///
/// **The claim, and nothing read out of it**: a call reaches its function only
/// through [`on_function`], so what it acts on is this claim's or nothing.
fn pci_claim(handle: RawHandle) -> Result<Arc<DeviceClaim>, crate::object::Refusal> {
    let claim = process::with_process_data(|data| {
        data.handles.get::<DeviceClaim>(handle, Rights::WRITE)
    })?;
    // A claim of another class is the wrong kind of thing here, not a missing
    // device: it names a device this call has no meaning for.
    match claim.class() {
        device::DeviceType::PciFunction => Ok(claim),
        other => Err(crate::object::HandleError::WrongType {
            held: other.class_name(),
            wanted: "a PCI function claim",
        }
        .into()),
    }
}

/// One `pcidev` call on the function `claim` drives, or [`SyscallError::Gone`]
/// for a claim whose last handle went under the call: a sibling thread closed
/// it, and whatever holds its slot now is not this caller's.
fn on_function<T>(
    claim: &DeviceClaim,
    call: impl FnOnce(&crate::pcidev::Binding) -> Result<T, SyscallError>,
) -> Result<T, SyscallError> {
    claim.pci(call).unwrap_or(Err(SyscallError::Gone))
}

/// One memory BAR of a claimed function, as an object to map.
pub(super) fn sys_device_bar_map(handle: RawHandle, index: u64) -> u64 {
    let claim = match pci_claim(handle) {
        Ok(claim) => claim,
        Err(e) => return e.refuse(),
    };
    let object = match on_function(&claim, |function| crate::pcidev::bar_object(function, index)) {
        Ok(object) => object,
        Err(e) => return e.to_u64(),
    };
    process::with_process_data(|data| {
        handle_result(ops::install(&mut data.handles, KObjectRef::SharedMem(object)))
    })
}

/// A firmware window the claim was granted, as an object its holder can map
/// and neither duplicate nor move: [`Rights::MAP`] and [`Rights::WRITE`]
/// alone. A refusal of what firmware set up goes back in the grant's word.
pub(super) fn sys_device_firmware_window(ctx: &SyscallContext, handle: RawHandle, window: u64, out: UserAddr) -> u64 {
    let claim = match pci_claim(handle) {
        Ok(claim) => claim,
        Err(e) => return e.refuse(),
    };
    let Some(window) = toyos_abi::pci::FirmwareWindow::from_raw(window) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    let len = core::mem::size_of::<toyos_abi::pci::WindowGrant>() as u64;
    let Some(mut out) = ctx.user_bytes_mut(out, len) else {
        return SyscallError::BadAddress.to_u64();
    };
    let mut grant = toyos_abi::pci::WindowGrant { shm: toyos_abi::HANDLE_INVALID, refused: 0, offset: 0, len: 0 };
    match on_function(&claim, |function| crate::pcidev::firmware_window(function, window)) {
        Err(e) => return e.to_u64(),
        Ok(Err(refused)) => grant.refused = refused as u32,
        Ok(Ok(crate::pcidev::Opened { object, offset, len })) => {
            let entry = crate::object::HandleEntry::new(KObjectRef::SharedMem(object), Rights::MAP.union(Rights::WRITE));
            let installed = process::with_process_data(|data| data.handles.install(entry));
            match installed {
                Ok(shm) => grant = toyos_abi::pci::WindowGrant { shm, refused: 0, offset, len },
                Err(_) => return SyscallError::ResourceExhausted.to_u64(),
            }
        }
    }
    // SAFETY: `grant` is a live reference readable for its own size, and the
    // layout assertion beside its declaration proves every byte of it is an
    // initialised field.
    let bytes = unsafe { core::slice::from_raw_parts(&grant as *const _ as *const u8, len as usize) };
    out.write_at(0, bytes);
    0
}

/// Memory the claimed function may reach and nothing else may.
pub(super) fn sys_device_dma_alloc(
    ctx: &SyscallContext,
    handle: RawHandle,
    bytes: u64,
    out: UserAddr,
) -> u64 {
    let claim = match pci_claim(handle) {
        Ok(claim) => claim,
        Err(e) => return e.refuse(),
    };
    // The output window is taken before the allocation: a bad address must
    // not leave a mapped grant the caller was never told the address of.
    let len = core::mem::size_of::<toyos_abi::pci::DmaGrant>() as u64;
    let Some(mut window) = ctx.user_bytes_mut(out, len) else {
        return SyscallError::BadAddress.to_u64();
    };
    let granted = on_function(&claim, |function| crate::pcidev::dma_alloc(function, bytes));
    let (memory, device_addr, granted) = match granted {
        Ok(grant) => grant,
        Err(e) => return e.to_u64(),
    };
    let installed = process::with_process_data(|data| {
        ops::install(&mut data.handles, KObjectRef::SharedMem(alloc::sync::Arc::clone(&memory)))
    });
    let shm = match installed {
        Ok(handle) => handle,
        // The grant goes back rather than staying mapped with nothing naming
        // it: a caller left holding neither the handle nor the quota would be
        // refused every later grant with no way out but dying.
        // A claim released since took the grant back itself.
        Err(e) => {
            claim.pci(|function| crate::pcidev::dma_undo(function, &memory));
            return e.to_u64();
        }
    };
    window.write_at(
        0,
        grant_bytes(&toyos_abi::pci::DmaGrant {
            shm,
            _pad: 0,
            device_addr,
            bytes: granted,
        }),
    );
    0
}

/// A region the caller holds, put into the claimed function's domain.
///
/// The caller must hold the region with [`Rights::MAP`] and [`Rights::WRITE`]:
/// a device writing into it is the same authority as a store through the
/// caller's own writable mapping, and no more. The output window is taken before the mapping, as
/// [`sys_device_dma_alloc`] takes its own: a bad address must not leave the
/// function reaching memory the caller was never told the address of.
pub(super) fn sys_device_dma_map(
    ctx: &SyscallContext,
    handle: RawHandle,
    region: RawHandle,
    out: UserAddr,
) -> u64 {
    let claim = match pci_claim(handle) {
        Ok(claim) => claim,
        Err(e) => return e.refuse(),
    };
    let memory = match process::with_process_data(|data| {
        data.handles.get::<crate::object::shm::SharedMemObject>(region, Rights::MAP.union(Rights::WRITE))
    }) {
        Ok(memory) => memory,
        Err(e) => {
            // `refuse` does not return: the claim is let go first.
            drop(claim);
            return e.refuse();
        }
    };
    let len = core::mem::size_of::<toyos_abi::pci::DmaMapping>() as u64;
    let Some(mut window) = ctx.user_bytes_mut(out, len) else {
        return SyscallError::BadAddress.to_u64();
    };
    let mapped = on_function(&claim, |function| crate::pcidev::dma_map(function, &memory));
    let (device_addr, bytes) = match mapped {
        Ok(mapped) => mapped,
        Err(e) => return e.to_u64(),
    };
    let mapping = toyos_abi::pci::DmaMapping { device_addr, bytes };
    // SAFETY: `mapping` is a live value readable for its own size, and the
    // layout assertion beside its declaration proves every byte of that width
    // is an initialised field.
    let raw = unsafe {
        core::slice::from_raw_parts(
            &mapping as *const _ as *const u8,
            core::mem::size_of::<toyos_abi::pci::DmaMapping>(),
        )
    };
    window.write_at(0, raw);
    0
}

/// Take back what [`sys_device_dma_map`] put at `device_addr`.
pub(super) fn sys_device_dma_unmap(handle: RawHandle, device_addr: u64) -> u64 {
    let claim = match pci_claim(handle) {
        Ok(claim) => claim,
        Err(e) => return e.refuse(),
    };
    // The region is let go here, with the claim's lock given back.
    match on_function(&claim, |function| crate::pcidev::dma_unmap(function, device_addr)) {
        Ok(_region) => 0,
        Err(e) => e.to_u64(),
    }
}

/// State the address width the claimed function issues.
pub(super) fn sys_device_dma_limit(handle: RawHandle, bits: u64) -> u64 {
    let claim = match pci_claim(handle) {
        Ok(claim) => claim,
        Err(e) => return e.refuse(),
    };
    match on_function(&claim, |function| crate::pcidev::dma_limit(function, bits)) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}

/// Let the claimed function master the bus.
pub(super) fn sys_device_dma_start(handle: RawHandle) -> u64 {
    let claim = match pci_claim(handle) {
        Ok(claim) => claim,
        Err(e) => return e.refuse(),
    };
    match on_function(&claim, crate::pcidev::dma_start) {
        Ok(()) => 0,
        Err(e) => e.to_u64(),
    }
}

/// The grant as the bytes that cross the boundary; its `const _` in `toyos-abi`
/// proves the layout has no gap, so nothing of this kernel's stack goes with it.
fn grant_bytes(grant: &toyos_abi::pci::DmaGrant) -> &[u8] {
    // SAFETY: `grant` is a live reference readable for its own size, and the
    // layout assertion beside its declaration proves every byte of that width
    // is an initialised field.
    unsafe {
        core::slice::from_raw_parts(
            grant as *const _ as *const u8,
            core::mem::size_of::<toyos_abi::pci::DmaGrant>(),
        )
    }
}
