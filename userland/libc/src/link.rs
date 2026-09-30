//! `dl_iterate_phdr` and `dladdr`, from the kernel's `SYS_QUERY_MODULES`
//! answer.
//!
//! `dl_iterate_phdr`'s contract is glibc's `dl_iterate_phdr(3)`, which the
//! LSB adopts: the executable first, named by the empty string, then every
//! library in load order; the walk stops at the first callback that returns
//! non-zero and returns that value, else 0. The walk is over one answer, so a
//! module a callback or another thread loads meanwhile is not visited.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

use toyos_abi::syscall;

use crate::elfsym::Image;

/// `struct dl_phdr_info`, as `link.h` declares it.
#[repr(C)]
pub struct DlPhdrInfo {
    dlpi_addr: u64,
    dlpi_name: *const u8,
    dlpi_phdr: *const u8,
    dlpi_phnum: u16,
}

type Callback = unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut u8) -> i32;

#[no_mangle]
pub unsafe extern "C" fn dl_iterate_phdr(callback: Callback, data: *mut u8) -> i32 {
    let answer = answer();
    let mut name = Vec::new();
    for (i, (module, path)) in syscall::modules(&answer).enumerate() {
        name.clear();
        if i > 0 {
            name.extend_from_slice(path);
        }
        name.push(0);
        let mut info = DlPhdrInfo {
            dlpi_addr: module.base,
            dlpi_name: name.as_ptr(),
            dlpi_phdr: module.phdr as *const u8,
            dlpi_phnum: u16::try_from(module.phnum).expect("SYS_QUERY_MODULES: a phnum past e_phnum's u16"),
        };
        // SAFETY: the caller's function, handed a record valid for the call.
        let stop = unsafe { callback(&mut info, core::mem::size_of::<DlPhdrInfo>(), data) };
        if stop != 0 {
            return stop;
        }
    }
    0
}

/// `Dl_info`, as `dlfcn.h` declares it.
#[repr(C)]
pub struct DlInfo {
    dli_fname: *const u8,
    dli_fbase: *mut u8,
    dli_sname: *const u8,
    dli_saddr: *mut u8,
}

/// The image holding `addr`, named by the path it was loaded from, and the
/// dynamic symbol of it that holds `addr` (`elfsym`), or null names where it
/// exports none; 0 where no image holds `addr`.
#[no_mangle]
pub unsafe extern "C" fn dladdr(addr: *const u8, info: *mut DlInfo) -> i32 {
    let answer = answer();
    for (module, path) in syscall::modules(&answer) {
        let image = Image {
            base: module.base,
            phdr: module.phdr as *const u8,
            phnum: usize::try_from(module.phnum).expect("SYS_QUERY_MODULES: a phnum past usize"),
        };
        // SAFETY: the kernel's description of an image this process has loaded.
        if !unsafe { image.contains(addr.addr() as u64) } {
            continue;
        }
        let symbol = unsafe { image.symbol(addr.addr() as u64) };
        unsafe {
            *info = DlInfo {
                dli_fname: named(module.base, path),
                dli_fbase: core::ptr::with_exposed_provenance_mut(image.start() as usize),
                dli_sname: symbol.map_or(core::ptr::null(), |(name, _)| name),
                dli_saddr: symbol.map_or(core::ptr::null_mut(), |(_, at)| core::ptr::with_exposed_provenance_mut(at as usize)),
            };
        }
        return 1;
    }
    0
}

/// A path `dladdr` has answered with, NUL-terminated, beside the base of the
/// image it named: kept for the life of the process, since `dli_fname` must
/// outlive every call, and each image's added once.
struct Named {
    base: u64,
    path: Vec<u8>,
    next: *mut Named,
}

static NAMED: AtomicPtr<Named> = AtomicPtr::new(core::ptr::null_mut());

/// `path` of the image at `base`, as a C string that outlives the process's
/// every call. Two threads naming a new image at once may each add it.
fn named(base: u64, path: &[u8]) -> *const u8 {
    let mut at = NAMED.load(Ordering::Acquire);
    while !at.is_null() {
        // SAFETY: a node once published is never freed or changed.
        let node = unsafe { &*at };
        if node.base == base && node.path[..node.path.len() - 1] == *path {
            return node.path.as_ptr();
        }
        at = node.next;
    }
    let mut owned = Vec::with_capacity(path.len() + 1);
    owned.extend_from_slice(path);
    owned.push(0);
    let node = Box::into_raw(Box::new(Named { base, path: owned, next: core::ptr::null_mut() }));
    loop {
        let head = NAMED.load(Ordering::Acquire);
        // SAFETY: `node` is this thread's until the exchange publishes it.
        unsafe { (*node).next = head };
        if NAMED.compare_exchange(head, node, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            // SAFETY: published, and never freed.
            return unsafe { (*node).path.as_ptr() };
        }
    }
}

/// One whole `SYS_QUERY_MODULES` answer: asked again while a `dlopen`
/// elsewhere grows it between the size and the read.
fn answer() -> Vec<u8> {
    let mut buf = Vec::new();
    loop {
        let need = syscall::query_modules(&mut buf).expect("SYS_QUERY_MODULES refused a buffer this process owns");
        if need <= buf.len() {
            buf.truncate(need);
            return buf;
        }
        buf.resize(need, 0);
    }
}
