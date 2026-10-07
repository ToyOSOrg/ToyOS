use core::sync::atomic::Ordering;

pub use super::common::Args;
use crate::ffi::{CStr, OsString};

pub fn args() -> Args {
    let argc = crate::sys::pal::ARGC.load(Ordering::Relaxed);
    let argv = core::ptr::with_exposed_provenance::<*const u8>(
        crate::sys::pal::ARGV.load(Ordering::Relaxed),
    );

    let mut vec = Vec::with_capacity(argc);

    for i in 0..argc {
        let ptr = unsafe { *argv.add(i) };
        if ptr.is_null() {
            break;
        }
        let cstr = unsafe { CStr::from_ptr(ptr.cast()) };
        vec.push(OsString::from(cstr.to_str().unwrap_or("")));
    }

    Args::new(vec)
}
