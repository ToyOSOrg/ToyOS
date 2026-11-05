//! `dl_iterate_phdr` and `dladdr`, from the process's module list
//! (`toyos::tls`).
//!
//! `dl_iterate_phdr`'s contract is glibc's `dl_iterate_phdr(3)`, which the
//! LSB adopts: the executable first, named by the empty string, then every
//! library in load order; the walk stops at the first callback that returns
//! non-zero and returns that value, else 0. A module a callback or another
//! thread loads meanwhile may or may not be visited. Neither allocates: each
//! module's name is the list's own.

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
    for (i, module) in toyos::tls::modules().enumerate() {
        let name = if i == 0 { c"" } else { module.name() };
        let mut info = DlPhdrInfo {
            dlpi_addr: module.image.base as u64,
            dlpi_name: name.as_ptr().cast(),
            dlpi_phdr: module.image.phdr as *const u8,
            dlpi_phnum: u16::try_from(module.image.phnum).expect("tls: a phnum past e_phnum's u16"),
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
    for module in toyos::tls::modules() {
        let image = Image { base: module.image.base as u64, phdr: module.image.phdr as *const u8, phnum: module.image.phnum };
        // SAFETY: the module list's description of an image this process has loaded.
        if !unsafe { image.contains(addr.addr() as u64) } {
            continue;
        }
        let symbol = unsafe { image.symbol(addr.addr() as u64) };
        unsafe {
            *info = DlInfo {
                dli_fname: module.name().as_ptr().cast(),
                dli_fbase: core::ptr::with_exposed_provenance_mut(image.start() as usize),
                dli_sname: symbol.map_or(core::ptr::null(), |(name, _)| name.as_ptr().cast()),
                dli_saddr: symbol.map_or(core::ptr::null_mut(), |(_, at)| core::ptr::with_exposed_provenance_mut(at as usize)),
            };
        }
        return 1;
    }
    0
}

