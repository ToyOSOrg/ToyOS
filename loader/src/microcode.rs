//! The microcode patch for the CPU this loader runs on: the file on ROOT that
//! `toyos_microcode` names for it, copied out of the image into `LoaderData`
//! pages of its own, which the kernel loads on every CPU and keeps out of its
//! allocator.
//!
//! Read from ROOT in memory after its slot's header has verified it, so the
//! patch is bytes that header covers. Page-aligned, which every alignment a
//! vendor's trigger asks is a divisor of (SDM Vol. 3A §12.11.6).

use alloc::string::ToString;

use bcachefs::{BlockBuf, BlockIO, BlockNum, DeviceError, FsError, Mounted, ReadOnly, TransferError};
use toyos_bootmap::BOOT_MAP_BYTES;

use crate::efi::{AllocateType, BootServices, PAGE_SIZE};
use crate::rootimage::RootImage;

/// The line every answer here is said on.
pub const SAID: &str = "Microcode:";

/// The patch for this CPU, or `None` where ROOT holds none for it or its
/// architecture loads none; either way said on [`SAID`]'s line.
pub fn find(bs: &BootServices, root: &RootImage) -> Option<&'static [u8]> {
    let cpu = match crate::arch::microcode_cpu()? {
        Ok(cpu) => cpu,
        Err(why) => {
            println!("{SAID} none handed over: {why}");
            return None;
        }
    };
    let path = cpu.path().to_string();
    let read = Mounted::<_, ReadOnly>::open(Image(root.bytes())).and_then(|fs| fs.read_file(&path));
    let bytes = match read {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => {
            println!("{SAID} /system/{path} is empty: none handed over");
            return None;
        }
        Err(FsError::NotFound) => {
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

/// ROOT's image as its filesystem reads it, blocks of it and none past it.
struct Image<'a>(&'a [u8]);

/// A block outside the image, or a write to it.
struct Outside;

impl TransferError for Outside {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

impl BlockIO for Image<'_> {
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
        let bytes = usize::try_from(block.raw())
            .ok()
            .and_then(|block| block.checked_mul(buf.0.len()))
            .and_then(|at| self.0.get(at..at.checked_add(buf.0.len())?))
            .ok_or(DeviceError::classify(&Outside))?;
        buf.0.copy_from_slice(bytes);
        Ok(())
    }

    /// A `ReadOnly` mount has no path here.
    fn write_block(&self, _: BlockNum, _: &BlockBuf) -> Result<(), DeviceError> {
        Err(DeviceError::classify(&Outside))
    }

    fn block_count(&self) -> u64 {
        (self.0.len() / size_of::<BlockBuf>()) as u64
    }
}
