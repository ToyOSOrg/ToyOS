//! `dl_iterate_phdr`, from the kernel's `SYS_QUERY_MODULES` answer.
//!
//! The contract is glibc's `dl_iterate_phdr(3)`, which the LSB adopts: the
//! executable first, named by the empty string, then every library in load
//! order; the walk stops at the first callback that returns non-zero and
//! returns that value, else 0. The walk is over one answer, so a module a
//! callback or another thread loads meanwhile is not visited.

use alloc::vec::Vec;

use toyos_abi::syscall;

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
