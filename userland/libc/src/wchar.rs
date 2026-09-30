//! Wide characters, and the UTF-8 the one locale (`locale.rs`) encodes them
//! in. A multibyte conversion refuses what is not UTF-8 (`EILSEQ`): an
//! overlong form, a surrogate, a code point above U+10FFFF (`utf8.rs`). Only
//! reading UTF-8 has a state; writing it has none. The character classes are
//! the C locale's, which are ASCII's.

use core::ffi::VaList;
use core::ptr;

use crate::arch::WChar;
use crate::errno::{self, EILSEQ};
use crate::locale::Locale;
use crate::strtonum;
use crate::utf8::{Byte, MbState};

type WInt = i32;
const WEOF: WInt = -1;
const EOF: i32 = -1;

/// `(size_t)-1`: a conversion refused.
const INVALID: usize = usize::MAX;
/// `(size_t)-2`: a character cut short, its bytes so far in the state.
const INCOMPLETE: usize = usize::MAX - 1;

/// The state a caller that passes none uses, one per function as C says.
struct Internal(core::cell::UnsafeCell<MbState>);
unsafe impl Sync for Internal {}

static MBRTOWC_STATE: Internal = Internal(core::cell::UnsafeCell::new(MbState::INITIAL));
static MBRLEN_STATE: Internal = Internal(core::cell::UnsafeCell::new(MbState::INITIAL));
static MBSRTOWCS_STATE: Internal = Internal(core::cell::UnsafeCell::new(MbState::INITIAL));

fn state_or(ps: *mut MbState, internal: &'static Internal) -> *mut MbState {
    if ps.is_null() { internal.0.get() } else { ps }
}

#[no_mangle]
pub unsafe extern "C" fn mbrtowc(pwc: *mut WChar, s: *const u8, n: usize, ps: *mut MbState) -> usize {
    let st = unsafe { &mut *state_or(ps, &MBRTOWC_STATE) };
    if s.is_null() {
        if st.is_partial() {
            *st = MbState::INITIAL;
            errno::set(EILSEQ);
            return INVALID;
        }
        return 0;
    }
    for i in 0..n {
        match st.feed(unsafe { *s.add(i) }) {
            Byte::Continues => {}
            Byte::Refused => {
                errno::set(EILSEQ);
                return INVALID;
            }
            Byte::Ends(cp) => {
                if !pwc.is_null() {
                    unsafe { *pwc = cp as WChar };
                }
                return if cp == 0 { 0 } else { i + 1 };
            }
        }
    }
    INCOMPLETE
}

/// `wc`'s UTF-8 bytes into `out`, and how many; `None` for what is not a
/// Unicode scalar value.
pub(crate) fn encode(wc: WChar, out: &mut [u8; 4]) -> Option<usize> {
    Some(char::from_u32(wc as u32)?.encode_utf8(out).len())
}

#[no_mangle]
pub unsafe extern "C" fn wcrtomb(s: *mut u8, wc: WChar, _ps: *mut MbState) -> usize {
    let mut buf = [0u8; 4];
    let wc = if s.is_null() { 0 } else { wc };
    let Some(len) = encode(wc, &mut buf) else {
        errno::set(EILSEQ);
        return INVALID;
    };
    if !s.is_null() {
        unsafe { ptr::copy_nonoverlapping(buf.as_ptr(), s, len) };
    }
    len
}

#[no_mangle]
pub unsafe extern "C" fn mbrlen(s: *const u8, n: usize, ps: *mut MbState) -> usize {
    unsafe { mbrtowc(ptr::null_mut(), s, n, state_or(ps, &MBRLEN_STATE)) }
}

#[no_mangle]
pub unsafe extern "C" fn mbsinit(ps: *const MbState) -> i32 {
    (ps.is_null() || !unsafe { &*ps }.is_partial()) as i32
}

#[no_mangle]
pub unsafe extern "C" fn btowc(c: i32) -> WInt {
    if (0..0x80).contains(&c) { c } else { WEOF }
}

#[no_mangle]
pub unsafe extern "C" fn wctob(c: WInt) -> i32 {
    if (0..0x80).contains(&c) { c } else { EOF }
}

#[no_mangle]
pub unsafe extern "C" fn mbtowc(pwc: *mut WChar, s: *const u8, n: usize) -> i32 {
    if s.is_null() {
        return 0;
    }
    let mut st = MbState::INITIAL;
    match unsafe { mbrtowc(pwc, s, n, &mut st) } {
        INVALID | INCOMPLETE => {
            errno::set(EILSEQ);
            -1
        }
        len => len as i32,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mblen(s: *const u8, n: usize) -> i32 {
    unsafe { mbtowc(ptr::null_mut(), s, n) }
}

#[no_mangle]
pub unsafe extern "C" fn wctomb(s: *mut u8, wc: WChar) -> i32 {
    if s.is_null() {
        return 0;
    }
    let mut st = MbState::INITIAL;
    match unsafe { wcrtomb(s, wc, &mut st) } {
        INVALID => -1,
        len => len as i32,
    }
}

#[no_mangle]
pub unsafe extern "C" fn mbsnrtowcs(
    dst: *mut WChar,
    src: *mut *const u8,
    nms: usize,
    len: usize,
    ps: *mut MbState,
) -> usize {
    let st = state_or(ps, &MBSRTOWCS_STATE);
    let mut p = unsafe { *src };
    let mut left = nms;
    let mut count = 0;
    while dst.is_null() || count < len {
        if left == 0 {
            break;
        }
        let mut wc: WChar = 0;
        match unsafe { mbrtowc(&mut wc, p, left, st) } {
            0 => {
                if !dst.is_null() {
                    unsafe {
                        *dst.add(count) = 0;
                        *src = ptr::null();
                    }
                }
                return count;
            }
            INVALID => {
                if !dst.is_null() {
                    unsafe { *src = p };
                }
                return INVALID;
            }
            INCOMPLETE => {
                p = unsafe { p.add(left) };
                break;
            }
            used => {
                if !dst.is_null() {
                    unsafe { *dst.add(count) = wc };
                }
                count += 1;
                p = unsafe { p.add(used) };
                left -= used;
            }
        }
    }
    if !dst.is_null() {
        unsafe { *src = p };
    }
    count
}

#[no_mangle]
pub unsafe extern "C" fn mbsrtowcs(dst: *mut WChar, src: *mut *const u8, len: usize, ps: *mut MbState) -> usize {
    unsafe { mbsnrtowcs(dst, src, usize::MAX, len, ps) }
}

#[no_mangle]
pub unsafe extern "C" fn mbstowcs(dst: *mut WChar, src: *const u8, n: usize) -> usize {
    let mut src = src;
    let mut st = MbState::INITIAL;
    unsafe { mbsnrtowcs(dst, &mut src, usize::MAX, n, &mut st) }
}

#[no_mangle]
pub unsafe extern "C" fn wcsnrtombs(
    dst: *mut u8,
    src: *mut *const WChar,
    nwc: usize,
    len: usize,
    _ps: *mut MbState,
) -> usize {
    let mut p = unsafe { *src };
    let mut count = 0;
    for _ in 0..nwc {
        let wc = unsafe { *p };
        let mut buf = [0u8; 4];
        let Some(n) = encode(wc, &mut buf) else {
            if !dst.is_null() {
                unsafe { *src = p };
            }
            errno::set(EILSEQ);
            return INVALID;
        };
        if !dst.is_null() {
            if count + n > len {
                break;
            }
            unsafe { ptr::copy_nonoverlapping(buf.as_ptr(), dst.add(count), n) };
        }
        if wc == 0 {
            if !dst.is_null() {
                unsafe { *src = ptr::null() };
            }
            return count;
        }
        count += n;
        p = unsafe { p.add(1) };
    }
    if !dst.is_null() {
        unsafe { *src = p };
    }
    count
}

#[no_mangle]
pub unsafe extern "C" fn wcsrtombs(dst: *mut u8, src: *mut *const WChar, len: usize, ps: *mut MbState) -> usize {
    unsafe { wcsnrtombs(dst, src, usize::MAX, len, ps) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstombs(dst: *mut u8, src: *const WChar, n: usize) -> usize {
    let mut src = src;
    let mut st = MbState::INITIAL;
    unsafe { wcsnrtombs(dst, &mut src, usize::MAX, n, &mut st) }
}

// Wide strings

#[no_mangle]
pub unsafe extern "C" fn wcslen(s: *const WChar) -> usize {
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn wcscpy(dst: *mut WChar, src: *const WChar) -> *mut WChar {
    unsafe { wmemcpy(dst, src, wcslen(src) + 1) }
}

#[no_mangle]
pub unsafe extern "C" fn wcsncpy(dst: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    let mut i = 0;
    while i < n && unsafe { *src.add(i) } != 0 {
        unsafe { *dst.add(i) = *src.add(i) };
        i += 1;
    }
    unsafe { wmemset(dst.add(i), 0, n - i) };
    dst
}

#[no_mangle]
pub unsafe extern "C" fn wcscat(dst: *mut WChar, src: *const WChar) -> *mut WChar {
    unsafe { wcscpy(dst.add(wcslen(dst)), src) };
    dst
}

#[no_mangle]
pub unsafe extern "C" fn wcsncat(dst: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    let end = unsafe { dst.add(wcslen(dst)) };
    let mut i = 0;
    while i < n && unsafe { *src.add(i) } != 0 {
        unsafe { *end.add(i) = *src.add(i) };
        i += 1;
    }
    unsafe { *end.add(i) = 0 };
    dst
}

#[no_mangle]
pub unsafe extern "C" fn wcscmp(a: *const WChar, b: *const WChar) -> i32 {
    unsafe { wcsncmp(a, b, usize::MAX) }
}

#[no_mangle]
pub unsafe extern "C" fn wcsncmp(a: *const WChar, b: *const WChar, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (*a.add(i), *b.add(i)) };
        if x != y {
            return if x < y { -1 } else { 1 };
        }
        if x == 0 {
            break;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn wcscoll(a: *const WChar, b: *const WChar) -> i32 {
    unsafe { wcscmp(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn wcscoll_l(a: *const WChar, b: *const WChar, _loc: *mut Locale) -> i32 {
    unsafe { wcscmp(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn wcsxfrm(dst: *mut WChar, src: *const WChar, n: usize) -> usize {
    let len = unsafe { wcslen(src) };
    if len < n {
        unsafe { wmemcpy(dst, src, len + 1) };
    }
    len
}

#[no_mangle]
pub unsafe extern "C" fn wcsxfrm_l(dst: *mut WChar, src: *const WChar, n: usize, _loc: *mut Locale) -> usize {
    unsafe { wcsxfrm(dst, src, n) }
}

#[no_mangle]
pub unsafe extern "C" fn wcschr(s: *const WChar, c: WChar) -> *mut WChar {
    let mut p = s;
    loop {
        let x = unsafe { *p };
        if x == c {
            return p.cast_mut();
        }
        if x == 0 {
            return ptr::null_mut();
        }
        p = unsafe { p.add(1) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn wcsrchr(s: *const WChar, c: WChar) -> *mut WChar {
    let mut found = ptr::null_mut();
    let mut p = s;
    loop {
        let x = unsafe { *p };
        if x == c {
            found = p.cast_mut();
        }
        if x == 0 {
            return found;
        }
        p = unsafe { p.add(1) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn wcsspn(s: *const WChar, accept: *const WChar) -> usize {
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 && !unsafe { wcschr(accept, *s.add(n)) }.is_null() {
        n += 1;
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn wcscspn(s: *const WChar, reject: *const WChar) -> usize {
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 && unsafe { wcschr(reject, *s.add(n)) }.is_null() {
        n += 1;
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn wcspbrk(s: *const WChar, accept: *const WChar) -> *mut WChar {
    let p = unsafe { s.add(wcscspn(s, accept)) };
    if unsafe { *p } == 0 { ptr::null_mut() } else { p.cast_mut() }
}

#[no_mangle]
pub unsafe extern "C" fn wcsstr(haystack: *const WChar, needle: *const WChar) -> *mut WChar {
    let n = unsafe { wcslen(needle) };
    let mut p = haystack;
    loop {
        if unsafe { wmemcmp(p, needle, n) } == 0 {
            return p.cast_mut();
        }
        if unsafe { *p } == 0 {
            return ptr::null_mut();
        }
        p = unsafe { p.add(1) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn wcstok(s: *mut WChar, delim: *const WChar, save: *mut *mut WChar) -> *mut WChar {
    let mut p = if s.is_null() { unsafe { *save } } else { s };
    if p.is_null() {
        return ptr::null_mut();
    }
    p = unsafe { p.add(wcsspn(p, delim)) };
    if unsafe { *p } == 0 {
        unsafe { *save = ptr::null_mut() };
        return ptr::null_mut();
    }
    let end = unsafe { p.add(wcscspn(p, delim)) };
    unsafe {
        if *end == 0 {
            *save = ptr::null_mut();
        } else {
            *end = 0;
            *save = end.add(1);
        }
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn wmemchr(s: *const WChar, c: WChar, n: usize) -> *mut WChar {
    (0..n).map(|i| unsafe { s.add(i) }).find(|&p| unsafe { *p } == c).map_or(ptr::null_mut(), <*const WChar>::cast_mut)
}

#[no_mangle]
pub unsafe extern "C" fn wmemcmp(a: *const WChar, b: *const WChar, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (*a.add(i), *b.add(i)) };
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn wmemcpy(dst: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    unsafe { ptr::copy_nonoverlapping(src, dst, n) };
    dst
}

#[no_mangle]
pub unsafe extern "C" fn wmemmove(dst: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    unsafe { ptr::copy(src, dst, n) };
    dst
}

#[no_mangle]
pub unsafe extern "C" fn wmemset(dst: *mut WChar, c: WChar, n: usize) -> *mut WChar {
    for i in 0..n {
        unsafe { *dst.add(i) = c };
    }
    dst
}

// Numbers

#[no_mangle]
pub unsafe extern "C" fn wcstol(s: *const WChar, endptr: *mut *mut WChar, base: i32) -> i64 {
    unsafe { crate::misc::answer(s, strtonum::signed(s, base), endptr) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstoul(s: *const WChar, endptr: *mut *mut WChar, base: i32) -> u64 {
    unsafe { crate::misc::answer(s, strtonum::unsigned(s, base), endptr) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstoll(s: *const WChar, endptr: *mut *mut WChar, base: i32) -> i64 {
    unsafe { wcstol(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstoull(s: *const WChar, endptr: *mut *mut WChar, base: i32) -> u64 {
    unsafe { wcstoul(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstoimax(s: *const WChar, endptr: *mut *mut WChar, base: i32) -> i64 {
    unsafe { wcstol(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstoumax(s: *const WChar, endptr: *mut *mut WChar, base: i32) -> u64 {
    unsafe { wcstoul(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstod(s: *const WChar, endptr: *mut *mut WChar) -> f64 {
    unsafe { crate::misc::answer(s, strtonum::float::<WChar, f64>(s), endptr) }
}

#[no_mangle]
pub unsafe extern "C" fn wcstof(s: *const WChar, endptr: *mut *mut WChar) -> f32 {
    unsafe { crate::misc::answer(s, strtonum::float::<WChar, f32>(s), endptr) }
}

// Formatted output: the narrow engine's, read back as UTF-8.

#[no_mangle]
pub unsafe extern "C" fn swprintf(s: *mut WChar, n: usize, fmt: *const WChar, args: ...) -> i32 {
    unsafe { vswprintf(s, n, fmt, args) }
}

/// `vswprintf`: the format's wide characters as UTF-8 for `vsnprintf`, whose
/// output is decoded back into `s`. Like C's, it refuses (-1) an output that
/// does not fit in `n` wide characters with its terminator.
#[no_mangle]
pub unsafe extern "C" fn vswprintf(s: *mut WChar, n: usize, fmt: *const WChar, ap: VaList<'_>) -> i32 {
    let mut narrow = alloc::vec::Vec::new();
    let len = unsafe { wcslen(fmt) };
    for i in 0..=len {
        let mut buf = [0u8; 4];
        let Some(k) = encode(unsafe { *fmt.add(i) }, &mut buf) else {
            errno::set(EILSEQ);
            return -1;
        };
        narrow.extend_from_slice(&buf[..k]);
    }
    let bytes = unsafe { crate::printf::vsnprintf(ptr::null_mut(), 0, narrow.as_ptr(), ap.clone()) };
    if bytes < 0 {
        return -1;
    }
    let mut out = alloc::vec![0u8; bytes as usize + 1];
    unsafe { crate::printf::vsnprintf(out.as_mut_ptr(), out.len(), narrow.as_ptr(), ap) };
    let mut src = out.as_ptr();
    let mut st = MbState::INITIAL;
    let wide = unsafe { mbsnrtowcs(ptr::null_mut(), &mut src, bytes as usize, 0, &mut st) };
    if wide == INVALID || wide >= n {
        return -1;
    }
    let mut src = out.as_ptr();
    let mut st = MbState::INITIAL;
    unsafe { mbsnrtowcs(s, &mut src, bytes as usize, wide, &mut st) };
    unsafe { *s.add(wide) = 0 };
    wide as i32
}

// Classes and cases: the C locale's, ASCII's.

fn ascii(wc: WInt) -> Option<i32> {
    (0..0x80).contains(&wc).then_some(wc)
}

macro_rules! wide_class {
    ($($name:ident, $name_l:ident => $narrow:path;)*) => {$(
        #[no_mangle]
        pub extern "C" fn $name(wc: WInt) -> i32 {
            ascii(wc).map_or(0, |c| $narrow(c))
        }

        #[no_mangle]
        pub extern "C" fn $name_l(wc: WInt, _loc: *mut Locale) -> i32 {
            $name(wc)
        }
    )*};
}

wide_class! {
    iswalnum, iswalnum_l => crate::ctype::isalnum;
    iswalpha, iswalpha_l => crate::ctype::isalpha;
    iswblank, iswblank_l => crate::ctype::isblank;
    iswcntrl, iswcntrl_l => crate::ctype::iscntrl;
    iswdigit, iswdigit_l => crate::ctype::isdigit;
    iswgraph, iswgraph_l => crate::ctype::isgraph;
    iswlower, iswlower_l => crate::ctype::islower;
    iswprint, iswprint_l => crate::ctype::isprint;
    iswpunct, iswpunct_l => crate::ctype::ispunct;
    iswspace, iswspace_l => crate::ctype::isspace;
    iswupper, iswupper_l => crate::ctype::isupper;
    iswxdigit, iswxdigit_l => crate::ctype::isxdigit;
}

/// `wctype`'s names, in the order its nonzero answers number them.
const CLASSES: [&[u8]; 12] =
    [b"alnum", b"alpha", b"blank", b"cntrl", b"digit", b"graph", b"lower", b"print", b"punct", b"space", b"upper", b"xdigit"];

#[no_mangle]
pub unsafe extern "C" fn wctype(name: *const u8) -> u64 {
    let name = unsafe { core::ffi::CStr::from_ptr(name.cast()) }.to_bytes();
    CLASSES.iter().position(|&c| c == name).map_or(0, |i| i as u64 + 1)
}

#[no_mangle]
pub unsafe extern "C" fn wctype_l(name: *const u8, _loc: *mut Locale) -> u64 {
    unsafe { wctype(name) }
}

#[no_mangle]
pub extern "C" fn iswctype(wc: WInt, desc: u64) -> i32 {
    let class: fn(WInt) -> i32 = match desc {
        1 => |c| iswalnum(c),
        2 => |c| iswalpha(c),
        3 => |c| iswblank(c),
        4 => |c| iswcntrl(c),
        5 => |c| iswdigit(c),
        6 => |c| iswgraph(c),
        7 => |c| iswlower(c),
        8 => |c| iswprint(c),
        9 => |c| iswpunct(c),
        10 => |c| iswspace(c),
        11 => |c| iswupper(c),
        12 => |c| iswxdigit(c),
        _ => return 0,
    };
    class(wc)
}

#[no_mangle]
pub extern "C" fn iswctype_l(wc: WInt, desc: u64, _loc: *mut Locale) -> i32 {
    iswctype(wc, desc)
}

#[no_mangle]
pub extern "C" fn towupper(wc: WInt) -> WInt {
    ascii(wc).map_or(wc, |c| crate::ctype::toupper(c))
}

#[no_mangle]
pub extern "C" fn towlower(wc: WInt) -> WInt {
    ascii(wc).map_or(wc, |c| crate::ctype::tolower(c))
}

#[no_mangle]
pub extern "C" fn towupper_l(wc: WInt, _loc: *mut Locale) -> WInt {
    towupper(wc)
}

#[no_mangle]
pub extern "C" fn towlower_l(wc: WInt, _loc: *mut Locale) -> WInt {
    towlower(wc)
}

const TRANS_TOLOWER: u64 = 1;
const TRANS_TOUPPER: u64 = 2;

#[no_mangle]
pub unsafe extern "C" fn wctrans(name: *const u8) -> u64 {
    match unsafe { core::ffi::CStr::from_ptr(name.cast()) }.to_bytes() {
        b"tolower" => TRANS_TOLOWER,
        b"toupper" => TRANS_TOUPPER,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn wctrans_l(name: *const u8, _loc: *mut Locale) -> u64 {
    unsafe { wctrans(name) }
}

#[no_mangle]
pub extern "C" fn towctrans(wc: WInt, desc: u64) -> WInt {
    match desc {
        TRANS_TOLOWER => towlower(wc),
        TRANS_TOUPPER => towupper(wc),
        _ => wc,
    }
}

#[no_mangle]
pub extern "C" fn towctrans_l(wc: WInt, desc: u64, _loc: *mut Locale) -> WInt {
    towctrans(wc, desc)
}

// Wide streams: UTF-8 on the byte stream underneath.

use crate::stdio::FILE;

#[no_mangle]
pub unsafe extern "C" fn fgetwc(f: *mut FILE) -> WInt {
    let mut st = MbState::INITIAL;
    loop {
        let c = unsafe { crate::stdio::fgetc(f) };
        if c == EOF {
            if st.is_partial() {
                errno::set(EILSEQ);
            }
            return WEOF;
        }
        let byte = c as u8;
        let mut wc: WChar = 0;
        match unsafe { mbrtowc(&mut wc, &byte, 1, &mut st) } {
            INCOMPLETE => continue,
            INVALID => return WEOF,
            _ => return wc as WInt,
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn getwc(f: *mut FILE) -> WInt {
    unsafe { fgetwc(f) }
}

#[no_mangle]
pub unsafe extern "C" fn ungetwc(wc: WInt, f: *mut FILE) -> WInt {
    let mut buf = [0u8; 4];
    match encode(wc as WChar, &mut buf) {
        Some(n) if !f.is_null() && unsafe { crate::stdio::unget(f, &buf[..n]) } => wc,
        _ => WEOF,
    }
}

#[no_mangle]
pub unsafe extern "C" fn fputwc(wc: WChar, f: *mut FILE) -> WInt {
    let mut buf = [0u8; 4];
    let Some(n) = encode(wc, &mut buf) else {
        errno::set(EILSEQ);
        return WEOF;
    };
    if unsafe { crate::stdio::fwrite(buf.as_ptr(), 1, n, f) } == n { wc as WInt } else { WEOF }
}

#[no_mangle]
pub unsafe extern "C" fn putwc(wc: WChar, f: *mut FILE) -> WInt {
    unsafe { fputwc(wc, f) }
}
