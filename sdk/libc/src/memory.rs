use core::ptr;

// Beside std (`std-runtime`), the program's C allocator is std's copy.
#[cfg(not(feature = "std-runtime"))]
#[path = "malloc.rs"]
mod malloc;

#[cfg(not(feature = "std-runtime"))]
use crate::runtime::ALLOCATOR as HEAP;

#[cfg(not(feature = "std-runtime"))]
fn refused(why: malloc::Refusal) {
    crate::errno::set(match why {
        malloc::Refusal::NoMemory => crate::errcode::ENOMEM,
        malloc::Refusal::Alignment => crate::errcode::EINVAL,
    });
}

// Whichever copy of `malloc.rs` the program has.
unsafe extern "C" {
    pub fn malloc(size: usize) -> *mut u8;
    pub fn free(p: *mut u8);
    fn aligned_alloc(align: usize, size: usize) -> *mut u8;
}

#[no_mangle]
pub unsafe extern "C" fn posix_memalign(out: *mut *mut u8, align: usize, size: usize) -> i32 {
    if !align.is_power_of_two() || align % core::mem::size_of::<usize>() != 0 {
        return crate::errcode::EINVAL;
    }
    let p = unsafe { aligned_alloc(align, size) };
    if p.is_null() {
        return crate::errcode::ENOMEM;
    }
    unsafe { *out = p };
    0
}

// memcpy, memmove and memset are the architecture's (`arch`): Rust's
// ptr::copy_nonoverlapping, and a copying loop, are lowered to calls to memcpy.

// This libc spells C strings and buffers as `*const u8`, not `c_char`/`c_void`:
// same ABI, different element type from the declaration std links against.
#[allow(suspicious_runtime_symbol_definitions)]
#[no_mangle]
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    crate::arch::copy_forward(dest, src, n);
    dest
}

#[allow(suspicious_runtime_symbol_definitions)]
#[no_mangle]
pub unsafe extern "C" fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dest as usize) <= (src as usize) || (dest as usize) >= (src as usize) + n {
        crate::arch::copy_forward(dest, src, n);
    } else {
        // Overlap with dest after src: copy backwards.
        crate::arch::copy_backward(dest, src, n);
    }
    dest
}

#[allow(suspicious_runtime_symbol_definitions)]
#[no_mangle]
pub unsafe extern "C" fn memset(dest: *mut u8, c: i32, n: usize) -> *mut u8 {
    crate::arch::fill(dest, c as u8, n);
    dest
}

#[allow(suspicious_runtime_symbol_definitions)]
#[no_mangle]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let a = *s1.add(i);
        let b = *s2.add(i);
        if a != b {
            return a as i32 - b as i32;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn memchr(s: *const u8, c: i32, n: usize) -> *mut u8 {
    let c = c as u8;
    for i in 0..n {
        if unsafe { *s.add(i) } == c {
            return unsafe { s.add(i) as *mut u8 };
        }
    }
    ptr::null_mut()
}
