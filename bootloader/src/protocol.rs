//! Every protocol the loader opens, opened here: `clippy.toml` refuses both
//! `BootServices` openers anywhere else, because the attribute decides whose
//! driver is stopped. EXCLUSIVE calls `Stop` on every driver holding the
//! protocol BY_DRIVER (UEFI 2.11 §7.3.9, `OpenProtocol()`), and on
//! `GraphicsOutput` that is the firmware's graphics console, whose screen the
//! loader's own lines are on. [`get`] opens GET_PROTOCOL, which stops nothing;
//! [`exclusive`] opens only a protocol no firmware console drives.

use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::ProtocolPointer;
use uefi::table::boot::{BootServices, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::Handle;

/// A protocol this loader may hold EXCLUSIVE.
pub trait Exclusive: ProtocolPointer {}

impl Exclusive for LoadedImage {}
impl Exclusive for DevicePath {}
impl Exclusive for SimpleFileSystem {}

#[allow(clippy::disallowed_methods, reason = "`Exclusive` is the bound the refusal asks for")]
pub fn exclusive<P: Exclusive + ?Sized>(bs: &BootServices, handle: Handle) -> uefi::Result<ScopedProtocol<'_, P>> {
    bs.open_protocol_exclusive::<P>(handle)
}

#[allow(clippy::disallowed_methods, reason = "the one attribute it passes stops no driver")]
pub fn get<P: ProtocolPointer + ?Sized>(bs: &BootServices, handle: Handle) -> uefi::Result<ScopedProtocol<'_, P>> {
    // SAFETY: `open_protocol`'s obligation is that the handle and its protocol
    // stay installed until the `ScopedProtocol` drops. Nothing between the two
    // can uninstall either: the loader is the one image running, it registers
    // no event callback, and it calls no boot service that connects or
    // disconnects a controller.
    unsafe {
        bs.open_protocol::<P>(
            OpenProtocolParams { handle, agent: bs.image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
}
