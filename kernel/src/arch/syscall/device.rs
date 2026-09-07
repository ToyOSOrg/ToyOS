//! Syscalls that drive a claimed device.
//!
//! [`holds_claim`] checks a claim's class against what the syscall requires;
//! device register access and protocol stay in the device's own code.

use crate::object::{ops, KObjectRef};
use crate::user_ptr::SyscallContext;
use crate::UserAddr;
use crate::{device, process};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;

use super::handles::{demand_syscap, handle_result, with_object_ref};

/// Refuses `WrongType` unless the handle's claim is on `class`.
pub(super) fn holds_claim(
    h: RawHandle,
    class: device::DeviceType,
) -> Result<(), crate::object::HandleError> {
    let held = with_object_ref(h, Rights::WRITE, |object| match object {
        KObjectRef::Device(d) => Ok(d.class()),
        other => Err(crate::object::HandleError::WrongType {
            held: other.kind(),
            wanted: "Device",
        }),
    })??;
    if held == class {
        Ok(())
    } else {
        Err(crate::object::HandleError::WrongType {
            held: held.class_name(),
            wanted: class.class_name(),
        })
    }
}

/// The register-access stub a device claim's class selects.
enum RegTarget {
    Hda,
    VirtioSound,
    /// A claimed PCI function's own config space, **read-only**.
    ///
    /// There is no writing counterpart and that is the point: a driver cannot
    /// find its own registers without its capability chain — virtio's four
    /// config windows are named by vendor capabilities and nothing else — while
    /// every write config space takes is a decision this kernel keeps. Bus
    /// mastering, the BARs and the MSI-X control word are all writes, and
    /// refusing them is what stops a holder aiming its device at memory or its
    /// interrupt at a vector.
    PciConfig(usize),
}

/// Reads or writes one register of the claimed device; the device owns its allow-list.
pub(super) fn sys_device_reg(handle: RawHandle, offset: u64, width: u64, value: Option<u64>) -> u64 {
    let Some(width) = toyos_abi::syscall::RegWidth::from_raw(width) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    let target = process::with_process_data(|data| {
        data.handles
            .get::<crate::object::device::DeviceClaim>(handle, Rights::NONE)
            .map(|claim| match claim.class() {
                device::DeviceType::HdaAudio => Some(RegTarget::Hda),
                device::DeviceType::VirtioSound => Some(RegTarget::VirtioSound),
                device::DeviceType::PciFunction => claim.pci_slot().map(RegTarget::PciConfig),
                _ => None,
            })
    });
    // `refuse` must run with nothing held; the guard above has already been dropped.
    let target = match target {
        Ok(t) => t,
        Err(e) => return e.refuse(),
    };
    // A claim with no register stub answers `NotSupported`, distinct from no such device.
    let Some(target) = target else {
        return SyscallError::NotSupported.to_u64();
    };
    match value {
        None => {
            let read = match target {
                RegTarget::Hda => crate::drivers::hda::reg_read(offset, width),
                RegTarget::VirtioSound => crate::drivers::virtio_sound::reg_read(offset, width),
                // The one place a caller's offset becomes an access: the
                // witness `pcidev::config_window` answers is the only thing
                // the register read takes, so an unchecked number cannot
                // reach the register file.
                RegTarget::PciConfig(slot) => match crate::pcidev::config_window(offset, width) {
                    Ok(at) => crate::pcidev::config_read(slot, at, width),
                    Err(_) => Err(SyscallError::InvalidArgument),
                },
            };
            match read {
                Ok(v) => v as u64,
                Err(e) => e.to_u64(),
            }
        }
        Some(value) => match u32::try_from(value) {
            Ok(value) => {
                let written = match target {
                    RegTarget::Hda => crate::drivers::hda::reg_write(offset, width, value),
                    RegTarget::VirtioSound => {
                        crate::drivers::virtio_sound::reg_write(offset, width, value)
                    }
                    // Config space has no write path from userland at all.
                    RegTarget::PciConfig(_) => Err(SyscallError::NotSupported),
                };
                match written {
                    Ok(()) => 0,
                    Err(e) => e.to_u64(),
                }
            }
            Err(_) => SyscallError::InvalidArgument.to_u64(),
        },
    }
}

/// Mints a device claim, gated on a `SysCap` carrying [`Rights::DEVICE`].
///
/// `selector` says which device where the class alone does not — a PCI
/// function's vendor and device id — and is ignored by every class that names
/// at most one device on the machine.
pub(super) fn sys_device_claim(syscap: RawHandle, class: u64, selector: u64) -> u64 {
    let Some(class) = device::DeviceType::from_raw(class) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    if let Err(e) = demand_syscap(syscap, Rights::DEVICE) {
        return e.refuse();
    }
    // Each refusal keeps its own word: init logs what it could not mint, and
    // "this machine has none" is a configuration while the rest are faults.
    let claim = match device::try_claim(class, selector) {
        Ok(c) => c,
        Err(device::ClaimError::Absent) => return SyscallError::NotFound.to_u64(),
        Err(device::ClaimError::Owned) => return SyscallError::AlreadyExists.to_u64(),
        Err(device::ClaimError::Ambiguous) => return SyscallError::InvalidArgument.to_u64(),
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

/// The `pcidev` slot a claim handle names, with the right every substrate call
/// demands.
///
/// [`Rights::WRITE`] for all three: each one gives the holder control of the
/// device — a register window, memory the device can reach, its interrupt — and
/// a claim carries no `DUP`, so no narrower handle to one can exist.
fn pci_slot(handle: RawHandle) -> Result<usize, crate::object::Refusal> {
    let slot = process::with_process_data(|data| {
        data.handles
            .get::<crate::object::device::DeviceClaim>(handle, Rights::WRITE)
            .map(|claim| claim.pci_slot())
    })?;
    // A claim of another class is the wrong kind of thing here, not a missing
    // device: it names a device this call has no meaning for.
    slot.ok_or(crate::object::HandleError::WrongType {
        held: device::DeviceType::PciFunction.class_name(),
        wanted: "a PCI function claim",
    }
    .into())
}

/// One memory BAR of a claimed function, as an object to map.
pub(super) fn sys_device_bar_map(handle: RawHandle, index: u64) -> u64 {
    let slot = match pci_slot(handle) {
        Ok(slot) => slot,
        Err(e) => return e.refuse(),
    };
    let object = match crate::pcidev::bar_object(slot, index) {
        Ok(object) => object,
        Err(e) => return e.to_u64(),
    };
    process::with_process_data(|data| {
        handle_result(ops::install(&mut data.handles, KObjectRef::SharedMem(object)))
    })
}

/// Memory the claimed function may reach and nothing else may.
pub(super) fn sys_device_dma_alloc(
    ctx: &SyscallContext,
    handle: RawHandle,
    bytes: u64,
    out: UserAddr,
) -> u64 {
    let slot = match pci_slot(handle) {
        Ok(slot) => slot,
        Err(e) => return e.refuse(),
    };
    // The output window is taken before the allocation, the same order
    // `sys_gpu_reset_scanout` takes it in: a bad address must not leave a
    // mapped grant the caller was never told the address of.
    let len = core::mem::size_of::<toyos_abi::pci::DmaGrant>() as u64;
    let Some(mut window) = ctx.user_bytes_mut(out, len) else {
        return SyscallError::BadAddress.to_u64();
    };
    let (memory, device_addr, granted) = match crate::pcidev::dma_alloc(slot, bytes) {
        Ok(grant) => grant,
        Err(e) => return e.to_u64(),
    };
    let installed = process::with_process_data(|data| {
        ops::install(&mut data.handles, KObjectRef::SharedMem(memory))
    });
    let shm = match installed {
        Ok(handle) => handle,
        // The grant goes back rather than staying mapped with nothing naming
        // it: a caller left holding neither the handle nor the quota would be
        // refused every later grant with no way out but dying.
        Err(e) => {
            crate::pcidev::dma_undo(slot);
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

/// Publishes a new mode with fresh buffer handles; the claim's old handles keep working.
pub(super) fn sys_gpu_reset_scanout(
    ctx: &SyscallContext,
    claim_h: RawHandle,
    gpu_info: crate::gpu::GpuInfo,
    info_out: UserAddr,
) -> u64 {
    let crate::gpu::GpuInfo { scanout, cursor, width, height, stride, pixel_format, flags } =
        gpu_info;
    let screen = device::Screen {
        info: toyos_abi::FramebufferInfo {
            scanout: [toyos_abi::HANDLE_INVALID; 2],
            cursor: toyos_abi::HANDLE_INVALID,
            width,
            height,
            stride,
            pixel_format,
            flags,
        },
        scanout,
        cursor,
    };
    device::set_framebuffer_info(screen.clone());
    // The output window is taken before the mint: a bad address must not leave a
    // resolution, or fresh buffer handles, the caller was never handed. Length is `FramebufferInfo`'s.
    let len = core::mem::size_of::<toyos_abi::FramebufferInfo>() as u64;
    let Some(mut out) = ctx.user_bytes_mut(info_out, len) else {
        return SyscallError::BadAddress.to_u64();
    };
    let minted = process::with_process_data(|data| {
        let claim = data
            .handles
            .get::<crate::object::device::DeviceClaim>(claim_h, Rights::WRITE)?;
        Ok::<_, crate::object::Refusal>(
            claim.remint(&mut data.handles, device::framebuffer_info(screen))?,
        )
    });
    let minted = match minted {
        Ok(bytes) => bytes,
        Err(e) => return e.refuse(),
    };
    out.write_at(0, &minted);
    0
}
