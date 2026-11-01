//! The microcode patch for the CPU this loader runs on: the file on ROOT that
//! `toyos_microcode` names for it, copied out of the image into `LoaderData`
//! pages of its own, which the kernel loads on every CPU and keeps out of its
//! allocator.
//!
//! Read from ROOT in memory after its slot's header has verified it, so the
//! patch is bytes that header covers. Page-aligned, which every alignment a
//! vendor's trigger asks is a divisor of (SDM Vol. 3A §12.11.6).

use alloc::string::ToString;

use bcachefs::engine::{self, Error};
use toyos_bootmap::BOOT_MAP_BYTES;

use crate::efi::{AllocateType, BootServices, PAGE_SIZE};
use crate::payload::Payload;

/// The line every answer here is said on.
pub const SAID: &str = "Microcode:";

/// The patch for this CPU, or `None` where ROOT holds none for it or its
/// architecture loads none; either way said on [`SAID`]'s line.
pub fn find(bs: &BootServices, root: &Payload) -> Option<&'static [u8]> {
    let cpu = match crate::arch::microcode_cpu()? {
        Ok(cpu) => cpu,
        Err(why) => {
            println!("{SAID} none handed over: {why}");
            return None;
        }
    };
    let path = cpu.path().to_string();
    let bytes = match engine::read_file(root.bytes(), path.as_bytes()) {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => {
            println!("{SAID} /system/{path} is empty: none handed over");
            return None;
        }
        Err(Error::NotFound) => {
            println!("{SAID} ROOT holds no /system/{path}: none handed over");
            return None;
        }
        Err(e) => {
            println!("{SAID} /system/{path} would not read off ROOT ({e:?}): none handed over");
            return None;
        }
    };
    // Inside the boot map: the BSP loads it before `mm::init`, through that map.
    let at = match bs.allocate_pages(AllocateType::MaxAddress(BOOT_MAP_BYTES - 1), bytes.len().div_ceil(PAGE_SIZE)) {
        Ok(at) => at,
        Err(e) => {
            println!("{SAID} firmware would not give {} bytes for /system/{path} ({e:?}): none handed over", bytes.len());
            return None;
        }
    };
    // SAFETY: `at` is the first of the pages just allocated to this loader,
    // identity-mapped while boot services live, which nothing else holds and
    // this loader never frees: the kernel keeps them by the address it is
    // handed.
    let patch = unsafe { core::slice::from_raw_parts_mut(at as *mut u8, bytes.len()) };
    patch.copy_from_slice(&bytes);
    println!("{SAID} /system/{path}, {} bytes at {at:#x}, handed over", patch.len());
    Some(patch)
}
