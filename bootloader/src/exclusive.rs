//! The loader's one way to open a protocol EXCLUSIVE, and only a protocol no
//! firmware console drives: EXCLUSIVE calls `Stop` on every driver holding the
//! protocol BY_DRIVER (UEFI 2.11 §7.3.9, `OpenProtocol()`), and on
//! `GraphicsOutput` that is the firmware's graphics console, whose screen the
//! loader's own lines are on. `clippy.toml` refuses the direct call.

use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::ProtocolPointer;
use uefi::table::boot::{BootServices, ScopedProtocol};
use uefi::Handle;

/// A protocol this loader may hold EXCLUSIVE.
pub trait Exclusive: ProtocolPointer {}

impl Exclusive for LoadedImage {}
impl Exclusive for DevicePath {}
impl Exclusive for SimpleFileSystem {}

#[allow(clippy::disallowed_methods, reason = "`Exclusive` is the bound the refusal asks for")]
pub fn open<P: Exclusive + ?Sized>(bs: &BootServices, handle: Handle) -> uefi::Result<ScopedProtocol<'_, P>> {
    bs.open_protocol_exclusive::<P>(handle)
}
