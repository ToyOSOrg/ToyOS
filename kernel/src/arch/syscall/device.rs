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

/// Mints a device claim for `class`, gated on a `SysCap` carrying [`Rights::DEVICE`].
pub(super) fn sys_device_claim(syscap: RawHandle, class: u64) -> u64 {
    let Some(class) = device::DeviceType::from_raw(class) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    if let Err(e) = demand_syscap(syscap, Rights::DEVICE) {
        return e.refuse();
    }
    // `NotFound` is no such device; `AlreadyExists` is a claim already taken.
    let claim = match device::try_claim(class) {
        Ok(c) => c,
        Err(device::ClaimError::Absent) => return SyscallError::NotFound.to_u64(),
        Err(device::ClaimError::Owned) => return SyscallError::AlreadyExists.to_u64(),
    };
    process::with_process_data(|data| {
        handle_result(ops::install(&mut data.handles, KObjectRef::Device(claim)))
    })
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
