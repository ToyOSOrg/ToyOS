use alloc::vec;
use core::ffi::VaList;

use crate::conversion::{self, Sink};

/// Output buffer for printf family. Writes to a raw C buffer with optional
/// capacity limit, and counts every byte it was given, as C's return value
/// does, whether or not it fit.
struct BufWriter {
    buf: *mut u8,
    pos: usize,
    cap: usize, // usize::MAX = unlimited (sprintf)
    /// A wide argument that is no Unicode scalar value: C's `EILSEQ`.
    refused: bool,
}

impl Sink for BufWriter {
    fn put(&mut self, b: u8) {
        if !self.buf.is_null() && self.pos + 1 < self.cap {
            unsafe { *self.buf.add(self.pos) = b; }
        }
        self.pos += 1;
    }
}

/// Core printf engine. Parses the format string as a byte slice.
unsafe fn do_printf(buf: *mut u8, n: usize, fmt: *const u8, ap: &mut VaList<'_>) -> i32 {
    let fmt = core::slice::from_raw_parts(fmt, super::string::strlen(fmt));
    let mut w = BufWriter { buf, pos: 0, cap: n, refused: false };
    let mut i = 0;

    while i < fmt.len() {
        if fmt[i] != b'%' {
            w.put(fmt[i]);
            i += 1;
            continue;
        }
        let Some((spec, len)) = conversion::spec(&fmt[i + 1..], || ap.next_arg::<i32>()) else { break };
        i += 1 + len;

        let (width, left_align, precision, long) = (spec.width, spec.left, spec.precision, spec.wide);
        match spec.conversion {
            b'd' | b'i' => {
                let val: i64 = if long { ap.next_arg::<i64>() } else { ap.next_arg::<i32>() as i64 };
                conversion::signed(&mut w, &spec, val);
            }
            b'u' | b'o' | b'x' | b'X' => {
                let val: u64 = if long { ap.next_arg::<u64>() } else { ap.next_arg::<u32>() as u64 };
                conversion::unsigned(&mut w, &spec, val);
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                conversion::float(&mut w, &spec, ap.next_arg::<f64>());
            }
            b'c' if long => {
                let mut utf8 = [0u8; 4];
                match crate::wchar::encode(ap.next_arg::<i32>() as crate::arch::WChar, &mut utf8) {
                    Some(n) => write_padded_bytes(&mut w, &utf8[..n], width, left_align),
                    None => w.refused = true,
                }
            }
            b'c' => {
                let c = ap.next_arg::<i32>() as u8;
                write_padded_bytes(&mut w, &[c], width, left_align);
            }
            b's' if long => {
                let p: *const crate::arch::WChar = ap.next_arg::<*const crate::arch::WChar>();
                let mut bytes = alloc::vec::Vec::new();
                if p.is_null() {
                    bytes.extend_from_slice(b"(null)");
                }
                let mut k = 0;
                while !p.is_null() && *p.add(k) != 0 {
                    let mut utf8 = [0u8; 4];
                    let Some(n) = crate::wchar::encode(*p.add(k), &mut utf8) else {
                        w.refused = true;
                        break;
                    };
                    if precision.is_some_and(|prec| bytes.len() + n > prec) {
                        break;
                    }
                    bytes.extend_from_slice(&utf8[..n]);
                    k += 1;
                }
                write_padded_bytes(&mut w, &bytes, width, left_align);
            }
            b's' => {
                let p: *const u8 = ap.next_arg::<*const u8>();
                if p.is_null() {
                    write_padded_bytes(&mut w, b"(null)", width, left_align);
                } else {
                    let len = super::string::strlen(p);
                    let actual_len = precision.map_or(len, |prec| prec.min(len));
                    write_padded_bytes(&mut w, core::slice::from_raw_parts(p, actual_len), width, left_align);
                }
            }
            b'p' => {
                let p: *const u8 = ap.next_arg::<*const u8>();
                let mut hex = [0u8; 22];
                let hex = conversion::digits(p as u64, 16, false, &mut hex);
                let mut tmp = [0u8; 18];
                tmp[..2].copy_from_slice(b"0x");
                tmp[2..2 + hex.len()].copy_from_slice(hex);
                write_padded_bytes(&mut w, &tmp[..2 + hex.len()], width, left_align);
            }
            b'%' => w.put(b'%'),
            other => {
                w.put(b'%');
                w.put(other);
            }
        }
    }

    if !buf.is_null() && n > 0 {
        *buf.add(w.pos.min(n - 1)) = 0;
    }

    if w.refused {
        crate::errno::set(crate::errcode::EILSEQ);
        return -1;
    }
    w.pos as i32
}

/// Hand `take` the whole of what `fmt` formats to, NUL-terminated, formatted
/// into a buffer on the stack, and again on the heap for an output that
/// buffer cannot hold: its length, or -1 when the format or `take` refuses.
unsafe fn formatted(fmt: *const u8, ap: VaList<'_>, take: impl FnOnce(&[u8]) -> bool) -> i32 {
    let mut stack = [0u8; 4096];
    let n = do_printf(stack.as_mut_ptr(), stack.len(), fmt, &mut ap.clone());
    if n < 0 {
        return n;
    }
    let len = n as usize;
    let taken = if len < stack.len() {
        take(&stack[..=len])
    } else {
        let mut whole = vec![0u8; len + 1];
        let mut ap = ap;
        do_printf(whole.as_mut_ptr(), whole.len(), fmt, &mut ap);
        take(&whole)
    };
    if taken { n } else { -1 }
}

/// Write the whole of what `fmt` formats to to `f`.
unsafe fn print_to(f: *mut super::stdio::FILE, fmt: *const u8, ap: VaList<'_>) -> i32 {
    formatted(fmt, ap, |bytes| {
        super::stdio::fwrite(bytes.as_ptr(), 1, bytes.len() - 1, f);
        true
    })
}

#[no_mangle]
pub unsafe extern "C" fn printf(fmt: *const u8, args: ...) -> i32 {
    print_to(super::stdio::stdout, fmt, args)
}

#[no_mangle]
pub unsafe extern "C" fn fprintf(f: *mut super::stdio::FILE, fmt: *const u8, args: ...) -> i32 {
    print_to(f, fmt, args)
}

#[no_mangle]
pub unsafe extern "C" fn sprintf(buf: *mut u8, fmt: *const u8, mut args: ...) -> i32 {
    do_printf(buf, usize::MAX, fmt, &mut args)
}

#[no_mangle]
pub unsafe extern "C" fn snprintf(buf: *mut u8, n: usize, fmt: *const u8, mut args: ...) -> i32 {
    do_printf(buf, n, fmt, &mut args)
}

#[no_mangle]
pub unsafe extern "C" fn vsnprintf(buf: *mut u8, n: usize, fmt: *const u8, mut ap: VaList<'_>) -> i32 {
    do_printf(buf, n, fmt, &mut ap)
}

#[no_mangle]
pub unsafe extern "C" fn vfprintf(f: *mut super::stdio::FILE, fmt: *const u8, ap: VaList<'_>) -> i32 {
    print_to(f, fmt, ap)
}

#[no_mangle]
pub unsafe extern "C" fn vprintf(fmt: *const u8, ap: VaList<'_>) -> i32 {
    vfprintf(super::stdio::stdout, fmt, ap)
}

#[no_mangle]
pub unsafe extern "C" fn vsprintf(buf: *mut u8, fmt: *const u8, ap: VaList<'_>) -> i32 {
    vsnprintf(buf, usize::MAX, fmt, ap)
}

#[no_mangle]
pub unsafe extern "C" fn sscanf(input: *const u8, fmt: *const u8, mut args: ...) -> i32 {
    let input = core::slice::from_raw_parts(input, super::string::strlen(input));
    let fmt = core::slice::from_raw_parts(fmt, super::string::strlen(fmt));
    let mut si = 0usize;
    let mut fi = 0usize;
    let mut matched = 0i32;

    while fi < fmt.len() && si < input.len() {
        if fmt[fi] == b'%' {
            fi += 1;
            if fi >= fmt.len() { break; }
            match fmt[fi] {
                b'd' => {
                    let p: *mut i32 = args.next_arg::<*mut i32>();
                    while si < input.len() && (input[si] as char).is_ascii_whitespace() { si += 1; }
                    let mut endptr: *mut u8 = core::ptr::null_mut();
                    let val = super::misc::strtol(input.as_ptr().add(si), &mut endptr, 10);
                    let consumed = endptr as usize - input.as_ptr().add(si) as usize;
                    if consumed == 0 { break; }
                    *p = val as i32;
                    si += consumed;
                    matched += 1;
                }
                b's' => {
                    let p: *mut u8 = args.next_arg::<*mut u8>();
                    while si < input.len() && (input[si] as char).is_ascii_whitespace() { si += 1; }
                    let mut j = 0;
                    while si < input.len() && !(input[si] as char).is_ascii_whitespace() {
                        *p.add(j) = input[si];
                        j += 1;
                        si += 1;
                    }
                    *p.add(j) = 0;
                    matched += 1;
                }
                _ => break,
            }
            fi += 1;
        } else if (fmt[fi] as char).is_ascii_whitespace() {
            while si < input.len() && (input[si] as char).is_ascii_whitespace() { si += 1; }
            fi += 1;
        } else {
            if input[si] != fmt[fi] { break; }
            si += 1;
            fi += 1;
        }
    }
    matched
}

/// `bytes`, space-padded to `width` on the side `left` says.
fn write_padded_bytes(w: &mut BufWriter, bytes: &[u8], width: usize, left: bool) {
    let pad = width.saturating_sub(bytes.len());
    if !left {
        (0..pad).for_each(|_| w.put(b' '));
    }
    bytes.iter().for_each(|&b| w.put(b));
    if left {
        (0..pad).for_each(|_| w.put(b' '));
    }
}

#[no_mangle]
pub unsafe extern "C" fn vasprintf(out: *mut *mut u8, fmt: *const u8, ap: VaList<'_>) -> i32 {
    formatted(fmt, ap, |bytes| {
        let buf = super::memory::malloc(bytes.len());
        if !buf.is_null() {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
            *out = buf;
        }
        !buf.is_null()
    })
}

#[no_mangle]
pub unsafe extern "C" fn asprintf(out: *mut *mut u8, fmt: *const u8, args: ...) -> i32 {
    vasprintf(out, fmt, args)
}
