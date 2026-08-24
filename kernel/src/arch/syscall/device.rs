//! The calls a claimed device is driven through.
//!
//! **The handle is the authority and the class is what it is a claim on.**
//! [`holds_claim`] is the gate every one of them shares: a process holding the
//! NIC has no more business setting the resolution than one holding nothing, so
//! a claim of the wrong class is refused as the wrong-typed handle it is.
//!
//! Nothing here knows a codec from a virtqueue. [`sys_device_reg`] resolves the
//! claim, asks the device behind it, and the device owns its own register
//! allow-list — which is the test for this being a device-register call rather
//! than a device protocol smuggled back into the syscall table.

use crate::object::{ops, KObjectRef};
use crate::user_ptr::SyscallContext;
use crate::UserAddr;
use crate::{device, process};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;

use super::handles::{demand_syscap, handle_result, with_object_ref};

/// The gate on every syscall that drives a claimed device.
///
/// **The handle is the authority, and the class is what it is a claim on.** A
/// process holding the NIC has no more business setting the resolution than one
/// holding nothing, which is why the class is checked and not merely the type —
/// the same `PermissionDenied` a wrong-typed handle gets from the table.
pub(super) fn holds_claim(
    h: RawHandle,
    class: device::DeviceType,
) -> Result<(), crate::object::HandleError> {
    // A claim on the wrong device is a wrong-typed handle and says so: what a
    // `DeviceClaim` *is* is a claim on one class, and a caller presenting the
    // NIC to `SYS_GPU_PRESENT` has the same bug as one presenting a pipe.
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

/// Which stub a claimed device handle names, and nothing about what it drives.
enum RegTarget {
    Hda,
    VirtioSound,
}

/// One register of a claimed device, read or written.
///
/// The handle is the authorization and the device behind it owns the
/// allow-list, so
/// this function knows nothing about codecs or virtqueues — which is the test
/// for it being a device-register call rather than a device protocol smuggled
/// back into the syscall table. Two stubs answer it
/// now, which is the first evidence for that claim rather than a restatement of
/// it.
pub(super) fn sys_device_reg(handle: RawHandle, offset: u64, width: u64, value: Option<u64>) -> u64 {
    let Some(width) = toyos_abi::syscall::RegWidth::from_raw(width) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    // **The table's own rule, and not one invented here.** This answered
    // `NotFound` for every way the handle could fail to resolve, so a process
    // naming a slot it never held — or one it had closed — was told its device
    // was missing, where `SYS_DEVICE_CLAIM` beside it ends the caller for the
    // same mistake (`object::HandleError::refuse_as_error`). `get` is asked
    // for the type, so a pipe presented here is the `WrongType` that it is.
    let target = process::with_process_data(|data| {
        data.handles
            .get::<crate::object::device::DeviceClaim>(handle, Rights::NONE)
            .map(|claim| match claim.class() {
                device::DeviceType::HdaAudio => Some(RegTarget::Hda),
                device::DeviceType::VirtioSound => Some(RegTarget::VirtioSound),
                _ => None,
            })
    });
    // Nothing held: `with_process_data` has given the guard up, which is what
    // `refuse` requires of the three kinds that do not come back from it.
    let target = match target {
        Ok(t) => t,
        Err(e) => return e.refuse(),
    };
    // A claim of a class with no register window. A different fact from "no
    // such device", and the one word left here that is not a lie.
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

/// Mint the claim for a device class, presenting a `SysCap` that carries
/// [`Rights::DEVICE`].
///
/// The kernel makes one such cap, at boot, for `/bin/init`, so the set of
/// processes that can reach this at all is exactly what init endowed. What
/// arbitrates between two programs wanting the framebuffer is then the
/// manifest, checked before the image was built, rather than which of them
/// started first.
pub(super) fn sys_device_claim(syscap: RawHandle, class: u64) -> u64 {
    let Some(class) = device::DeviceType::from_raw(class) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    if let Err(e) = demand_syscap(syscap, Rights::DEVICE) {
        return e.refuse();
    }
    // `NotFound` is a machine with no such device and nothing else: init endows
    // what it got and logs what it did not, which is a different answer from
    // `AlreadyExists` — a config the build-time gate should have refused.
    let claim = match device::try_claim(class) {
        Ok(c) => c,
        Err(device::ClaimError::Absent) => return SyscallError::NotFound.to_u64(),
        Err(device::ClaimError::Owned) => return SyscallError::AlreadyExists.to_u64(),
    };
    process::with_process_data(|data| {
        handle_result(ops::install(&mut data.handles, KObjectRef::Device(claim)))
    })
}

/// Publish a new mode: fresh buffer handles, and the claim's description
/// replaced so a later read answers with them.
///
/// **The handles the old description named keep working.** Their objects hold
/// the old pages, so a compositor can keep blitting the screen it has until it
/// has mapped the one it just asked for — where the token registry took the
/// mapping away on this CPU and shot down before the pages were reissued,
/// which is a revocation this design does not have and does not need.
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
    let Some(mut out) = ctx.user_bytes_mut(info_out, minted.len() as u64) else {
        return SyscallError::BadAddress.to_u64();
    };
    out.write_at(0, &minted);
    0
}
