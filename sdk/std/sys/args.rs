pub use super::common::Args;
use crate::ffi::{CStr, OsString};

pub fn args() -> Args {
    let (argc, argv) = toyos::startup::args();
    let vec = (0..argc)
        .map(|i| {
            // SAFETY: one of the block's `argc` arguments, a NUL-terminated string.
            let arg = unsafe { CStr::from_ptr((*argv.add(i)).cast()) };
            OsString::from(arg.to_str().unwrap_or(""))
        })
        .collect();
    Args::new(vec)
}
