//! A device claim, and the one console every kernel-spawned process starts on.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::SyscallError;
use toyos_abi::FramebufferInfo;

use crate::device::{Claim, DeviceType};

use super::handle::{HandleEntry, HandleTable};
use super::shm::SharedMemObject;
use super::{Held, KObjectRef, KObjectVariant, ObjectCore, ZeroHandles};

/// What the class answers when its holder reads it.
pub enum DeviceInfo {
    // Keyboard and mouse answer with events, not a description.
    Events,
    Framebuffer(FramebufferInfo, FramebufferBuffers),
    Nic(crate::net::NicInfo, Arc<SharedMemObject>),
    Hda(toyos_abi::hda::HdaInfo, Arc<SharedMemObject>),
    VirtioSound(toyos_abi::virtio_sound::VirtioSoundInfo, Arc<SharedMemObject>),
}

/// The two scanout buffers and the cursor plane.
pub struct FramebufferBuffers {
    pub scanout: [Arc<SharedMemObject>; 2],
    pub cursor: Arc<SharedMemObject>,
}

// MAP is required; DUP and TRANSFER let a daemon hand the buffer to another process.
const BUFFER_RIGHTS: Rights = Rights::MAP.union(Rights::DUP).union(Rights::TRANSFER);

fn install_buffer(
    table: &mut HandleTable,
    buffer: &Arc<SharedMemObject>,
) -> Result<RawHandle, SyscallError> {
    table
        .install(HandleEntry::new(KObjectRef::SharedMem(buffer.clone()), BUFFER_RIGHTS))
        .map_err(|_| SyscallError::ResourceExhausted)
}

impl DeviceInfo {
    /// The description as bytes, with a handle installed for every named buffer.
    fn mint(&self, table: &mut HandleTable) -> Result<Box<[u8]>, SyscallError> {
        Ok(match self {
            Self::Events => Box::new([]),
            Self::Framebuffer(info, buffers) => {
                let mut info = *info;
                info.scanout = [
                    install_buffer(table, &buffers.scanout[0])?,
                    install_buffer(table, &buffers.scanout[1])?,
                ];
                info.cursor = install_buffer(table, &buffers.cursor)?;
                info.as_bytes().into()
            }
            Self::Nic(info, dma) => {
                let mut info = *info;
                info.dma = install_buffer(table, dma)?;
                info.as_bytes().into()
            }
            Self::Hda(info, pcm) => {
                let mut info = *info;
                info.pcm = install_buffer(table, pcm)?;
                info.as_bytes().into()
            }
            Self::VirtioSound(info, dma) => {
                let mut info = *info;
                info.dma = install_buffer(table, dma)?;
                info.as_bytes().into()
            }
        })
    }
}

/// One process's exclusive hold on a device class.
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
    // Shares this lock with `info`: SYS_GPU_SET_RESOLUTION must replace both together.
    bytes: Option<Box<[u8]>>,
}

impl DeviceClaim {
    pub fn new(class: DeviceType, info: DeviceInfo, claim: Claim) -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            class,
            info_read: AtomicBool::new(false),
            described: crate::sync::Lock::new(Described { info, bytes: None }),
            reference: Held::new(claim),
        })
    }

    pub fn class(&self) -> DeviceType {
        self.class
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

    /// Replace the description, for a mode set that reallocated the buffers — old handles keep working; nothing is revoked.
    pub fn remint(
        &self,
        table: &mut HandleTable,
        info: DeviceInfo,
    ) -> Result<Box<[u8]>, SyscallError> {
        let mut described = self.described.lock();
        let minted = info.mint(table)?;
        described.info = info;
        described.bytes = Some(minted.clone());
        Ok(minted)
    }
}

// Released on last handle, not last Arc: a parked daemon can strand an Arc without releasing.
impl ZeroHandles for DeviceClaim {
    fn on_zero_handles(&self) {
        self.reference.release();
    }
}

/// One holder's view of the machine's serial console — each holder gets its own, never a shared handle.
pub struct ConsoleObject {
    pub(super) core: ObjectCore,
    // Lock order process_data -> line -> BackendGuard; taking them in reverse deadlocks.
    line: crate::sync::Lock<crate::drivers::serial::ConsoleLine>,
}

impl ConsoleObject {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            line: crate::sync::Lock::new(crate::drivers::serial::ConsoleLine::new()),
        })
    }

    /// Take a userland write, emitting every whole line it completes.
    pub fn write(&self, buf: &crate::user_ptr::UserBytes) {
        // Negative control: bypasses buffering so console_line_atomicity reds if this breaks.
        if crate::actuator::console_unbuffered() {
            crate::drivers::serial::write_console(buf);
            return;
        }
        self.line.lock().write(buf);
    }
}

// Flushes the partial line on last handle instead of dropping it.
impl Drop for ConsoleObject {
    fn drop(&mut self) {
        self.line.lock().finish();
    }
}
