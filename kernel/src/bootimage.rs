//! The payloads the loader hands the kernel beside its own ELF: the
//! supervisor's executable, the file server's and ROOT's bytes
//! (`toyos_abi::boot::BootImage`). The kernel parses none of them but the
//! supervisor's program headers, which it spawns the supervisor from; the
//! other two it hands the supervisor, whose file server serves `/system` out
//! of ROOT.
//!
//! [`init`] runs before `mm::init` and refuses the boot by name unless each
//! image is non-empty, page-aligned and whole pages inside one `LoaderData`
//! descriptor; `kernel_main` then refuses any two reserved regions that share
//! a byte. The loader is trusted and verified each image's hash, so a refusal
//! here is its bug, not an input to survive. The regions it returns are kept
//! out of the allocator and charged to no process, for the machine's life.
//!
//! [`objects`] makes each one a [`SharedMemObject`] over its pages
//! ([`SharedMemObject::boot`]), whose handles never carry `WRITE`
//! (`ops::initial_rights`, and every part of one inherits its source's
//! rights), so every mapping of it is read-only; `sys_shm_map` asserts it.

use alloc::sync::Arc;

use toyos_abi::boot::{BootImage, KernelArgs, MemoryMapEntry};
use toyos_rootimage::handoff::{held, Descriptor};

use crate::mm::{Region, PAGE_SIZE};
use crate::object::shm::SharedMemObject;

/// The three images, as the loader handed them and [`init`] judged them.
#[derive(Clone, Copy)]
pub struct Handed {
    supervisor: BootImage,
    fileserver: BootImage,
    root: BootImage,
}

/// Every image in the order [`init`] returns their regions, by name.
fn named(handed: &Handed) -> [(&'static str, BootImage); 3] {
    [("supervisor", handed.supervisor), ("fileserver", handed.fileserver), ("ROOT", handed.root)]
}

/// The pages `image` occupies.
fn pages(image: BootImage) -> Region {
    Region { start: image.addr, end: image.addr + image.len.next_multiple_of(PAGE_SIZE) }
}

/// Take the images out of the handoff, refusing the boot by name on one the
/// kernel may not keep, and answer them with their regions for `mm::init` to
/// withhold. Runs before `mm::init`, so it neither allocates nor logs.
pub fn init(args: &KernelArgs, map: &[MemoryMapEntry]) -> (Handed, [Region; 3]) {
    let handed = Handed { supervisor: args.supervisor_image, fileserver: args.fileserver_image, root: args.root_image };
    for (what, image) in named(&handed) {
        assert!(image.len != 0, "boot: the loader handed no {what} image");
        let rounded = image.len.checked_next_multiple_of(PAGE_SIZE);
        let kept = rounded.and_then(|len| {
            held(
                map.iter().map(|e| Descriptor { ty: e.uefi_type, start: e.start, end: e.end }),
                toyos_bootmap::EFI_LOADER_DATA,
                image.addr,
                len,
                PAGE_SIZE,
            )
        });
        assert!(
            kept.is_some(),
            "boot: the {what} image at {:#x}+{:#x} is not whole {PAGE_SIZE}-byte pages inside one LoaderData \
             descriptor",
            image.addr,
            image.len
        );
    }
    (handed, named(&handed).map(|(_, image)| pages(image)))
}

/// The objects the supervisor is spawned from and handed.
pub struct Objects {
    pub supervisor: Arc<SharedMemObject>,
    pub fileserver: Arc<SharedMemObject>,
    pub root: Arc<SharedMemObject>,
}

/// One object over each image, once `mm::init` has run, for the supervisor's
/// spawn.
pub fn objects(handed: Handed) -> Objects {
    let object = |image: BootImage| SharedMemObject::boot(image.addr, image.len).expect("boot: no heap for a boot image's object");
    for (what, image) in named(&handed) {
        log!("boot: {what} image {:#x}+{:#x}, kept read-only for the machine's life", image.addr, image.len);
    }
    Objects { supervisor: object(handed.supervisor), fileserver: object(handed.fileserver), root: object(handed.root) }
}
