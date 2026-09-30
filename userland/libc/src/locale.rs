//! The one locale: C's. Every name that means it (`C`, `POSIX`, `C.UTF-8`
//! and the empty default) names it, any other is refused (`ENOENT`), its
//! multibyte encoding is UTF-8 (`wchar.rs`), and every `_l` function answers
//! as its locale-free twin does.

use core::cell::Cell;
use core::ffi::{c_char, CStr};
use core::ptr;

use crate::errno::{self, EINVAL, ENOENT};

/// `locale_t` points at this, and only ever at the one there is.
#[repr(C)]
pub struct Locale {
    _only: u8,
}

static C_LOCALE: Locale = Locale { _only: 0 };

/// `LC_GLOBAL_LOCALE`, `(locale_t)-1`.
const GLOBAL: *mut Locale = usize::MAX as *mut Locale;

const LC_ALL: i32 = 6;
const LC_ALL_MASK: i32 = (1 << LC_ALL) - 1;

#[thread_local]
static CURRENT: Cell<*mut Locale> = Cell::new(GLOBAL);

fn c_locale() -> *mut Locale {
    ptr::addr_of!(C_LOCALE).cast_mut()
}

unsafe fn names_c(name: *const u8) -> bool {
    matches!(unsafe { CStr::from_ptr(name.cast()) }.to_bytes(), b"" | b"C" | b"POSIX" | b"C.UTF-8")
}

#[no_mangle]
pub unsafe extern "C" fn setlocale(category: i32, name: *const u8) -> *const u8 {
    if !(0..=LC_ALL).contains(&category) || (!name.is_null() && !unsafe { names_c(name) }) {
        return ptr::null();
    }
    c"C".as_ptr().cast()
}

#[no_mangle]
pub unsafe extern "C" fn newlocale(mask: i32, name: *const u8, _base: *mut Locale) -> *mut Locale {
    if mask & !LC_ALL_MASK != 0 || name.is_null() {
        errno::set(EINVAL);
        return ptr::null_mut();
    }
    if !unsafe { names_c(name) } {
        errno::set(ENOENT);
        return ptr::null_mut();
    }
    c_locale()
}

#[no_mangle]
pub unsafe extern "C" fn duplocale(_loc: *mut Locale) -> *mut Locale {
    c_locale()
}

#[no_mangle]
pub unsafe extern "C" fn freelocale(_loc: *mut Locale) {}

#[no_mangle]
pub unsafe extern "C" fn uselocale(new: *mut Locale) -> *mut Locale {
    let old = CURRENT.get();
    if !new.is_null() {
        CURRENT.set(new);
    }
    old
}

/// `struct lconv` as `include/locale.h` lays it out.
#[repr(C)]
pub struct Lconv {
    strings: [*const c_char; 10],
    chars: [c_char; 14],
}

struct CLconv(Lconv);

// SAFETY: every pointer is to a string literal, and nothing writes through it.
unsafe impl Sync for CLconv {}

/// The C locale's: a decimal point, and nothing else there is to say.
static C_LCONV: CLconv = CLconv(Lconv {
    strings: [c".".as_ptr(), c"".as_ptr(), c"".as_ptr(), c"".as_ptr(), c"".as_ptr(), c"".as_ptr(), c"".as_ptr(), c"".as_ptr(), c"".as_ptr(), c"".as_ptr()],
    chars: [c_char::MAX; 14],
});

#[no_mangle]
pub extern "C" fn localeconv() -> *mut Lconv {
    ptr::addr_of!(C_LCONV.0).cast_mut()
}

macro_rules! same_in_every_locale {
    ($($name:ident => $plain:path;)*) => {$(
        #[no_mangle]
        pub extern "C" fn $name(c: i32, _loc: *mut Locale) -> i32 {
            $plain(c)
        }
    )*};
}

same_in_every_locale! {
    isalnum_l => crate::ctype::isalnum;
    isalpha_l => crate::ctype::isalpha;
    isblank_l => crate::ctype::isblank;
    iscntrl_l => crate::ctype::iscntrl;
    isdigit_l => crate::ctype::isdigit;
    isgraph_l => crate::ctype::isgraph;
    islower_l => crate::ctype::islower;
    isprint_l => crate::ctype::isprint;
    ispunct_l => crate::ctype::ispunct;
    isspace_l => crate::ctype::isspace;
    isupper_l => crate::ctype::isupper;
    isxdigit_l => crate::ctype::isxdigit;
    toupper_l => crate::ctype::toupper;
    tolower_l => crate::ctype::tolower;
}

/// C's collation is byte order.
#[no_mangle]
pub unsafe extern "C" fn strcoll(a: *const u8, b: *const u8) -> i32 {
    unsafe { crate::string::strcmp(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn strcoll_l(a: *const u8, b: *const u8, _loc: *mut Locale) -> i32 {
    unsafe { strcoll(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn strxfrm(dst: *mut u8, src: *const u8, n: usize) -> usize {
    let len = unsafe { crate::string::strlen(src) };
    if len < n {
        unsafe { ptr::copy_nonoverlapping(src, dst, len + 1) };
    }
    len
}

#[no_mangle]
pub unsafe extern "C" fn strxfrm_l(dst: *mut u8, src: *const u8, n: usize, _loc: *mut Locale) -> usize {
    unsafe { strxfrm(dst, src, n) }
}

#[no_mangle]
pub unsafe extern "C" fn strftime_l(
    s: *mut u8,
    max: usize,
    fmt: *const u8,
    tm: *const crate::time::Tm,
    _loc: *mut Locale,
) -> usize {
    unsafe { crate::time::strftime(s, max, fmt, tm) }
}

#[no_mangle]
pub unsafe extern "C" fn strtod_l(s: *const u8, endptr: *mut *mut u8, _loc: *mut Locale) -> f64 {
    unsafe { crate::misc::strtod(s, endptr) }
}

#[no_mangle]
pub unsafe extern "C" fn strtof_l(s: *const u8, endptr: *mut *mut u8, _loc: *mut Locale) -> f32 {
    unsafe { crate::misc::strtof(s, endptr) }
}
