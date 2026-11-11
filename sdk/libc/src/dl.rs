//! `dlopen`, `dlsym`, `dlclose` and `dlerror` over the SDK's loader
//! (`toyos::dl`): a handle is a module's id, never null, and nothing leaves
//! the module list, so `dlclose` releases nothing.

use core::cell::Cell;
use core::fmt::Write;
use core::ptr;

use toyos::dl;

#[no_mangle]
pub unsafe extern "C" fn dlopen(path: *const u8, _flags: i32) -> *mut u8 {
    if path.is_null() {
        return ptr::null_mut();
    }
    let Ok(path) = core::str::from_utf8(crate::posix_io::c_str_to_bytes(path)) else {
        crate::errno::set(crate::errcode::ENOENT);
        return ptr::null_mut();
    };
    match dl::open(path) {
        Ok(module) => module.id() as *mut u8,
        Err(refused) => {
            refuse(format_args!("dlopen: {refused}"));
            crate::errno::set(crate::errcode::of(refused));
            ptr::null_mut()
        }
    }
}

/// The symbol `symbol` names: in the module `handle` names, any symbol it
/// defines; with `RTLD_DEFAULT` (null), the first module's that exports it.
#[no_mangle]
pub unsafe extern "C" fn dlsym(handle: *mut u8, symbol: *const u8) -> *mut u8 {
    if symbol.is_null() {
        return ptr::null_mut();
    }
    let Ok(name) = core::str::from_utf8(crate::posix_io::c_str_to_bytes(symbol)) else {
        return ptr::null_mut();
    };
    let found = match handle.is_null() {
        true => dl::symbol(None, name),
        false => dl::module(handle as usize).and_then(|module| dl::symbol(Some(module), name)),
    };
    found.map_or_else(
        || {
            refuse(format_args!("dlsym: no symbol `{name}`"));
            ptr::null_mut()
        },
        |at| at as *mut u8,
    )
}

/// Nothing leaves the module list: `0` for a handle `dlopen` answered.
#[no_mangle]
pub unsafe extern "C" fn dlclose(handle: *mut u8) -> i32 {
    if dl::module(handle as usize).is_some() { 0 } else { -1 }
}

/// Bytes of the calling thread's last refusal `dlerror` has not said yet,
/// NUL-terminated, cut to fit; 0 for none.
#[thread_local]
static UNSAID: Cell<usize> = Cell::new(0);

/// Where the refusal is written: valid until the thread's next `dlopen` or
/// `dlsym` refusal.
#[thread_local]
static mut TEXT: [u8; 256] = [0; 256];

/// The calling thread's own text, written once a refusal is formatted.
struct Text(usize);

impl Write for Text {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // SAFETY: the calling thread's own buffer; one byte is kept for the NUL.
        let buf = unsafe { &mut *(&raw mut TEXT) };
        let room = buf.len() - 1 - self.0;
        let n = s.len().min(room);
        buf[self.0..self.0 + n].copy_from_slice(&s.as_bytes()[..n]);
        self.0 += n;
        Ok(())
    }
}

/// Record `why` as the calling thread's refusal for `dlerror`.
fn refuse(why: core::fmt::Arguments<'_>) {
    let mut text = Text(0);
    let _ = text.write_fmt(why);
    // SAFETY: the calling thread's own buffer, `text.0` short of its end.
    unsafe { (*(&raw mut TEXT))[text.0] = 0 };
    UNSAID.set(text.0 + 1);
}

/// The last refusal not said yet; null for none.
#[no_mangle]
pub unsafe extern "C" fn dlerror() -> *const u8 {
    match UNSAID.replace(0) {
        0 => ptr::null(),
        // SAFETY: the calling thread's own buffer, which only it writes.
        _ => unsafe { (*(&raw const TEXT)).as_ptr() },
    }
}
