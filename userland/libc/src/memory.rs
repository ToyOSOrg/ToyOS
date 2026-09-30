use core::ptr;

// C malloc/free/calloc/realloc route through the Rust global allocator (dlmalloc).
// We store the allocation size in a header so C's free(ptr) can recover it.
//
// Linked beside std (`std-runtime`), std's own C allocator defines all four,
// and a second definition is a duplicate symbol: this library calls std's.
#[cfg(feature = "std-runtime")]
extern "C" {
    pub fn malloc(size: usize) -> *mut u8;
    pub fn free(p: *mut u8);
}

#[cfg(not(feature = "std-runtime"))]
mod backend {
    use core::alloc::Layout;
    use core::ptr;

    /// The alignment every block has at least, and the size of the two words
    /// in front of it: its size and its alignment.
    const MIN_ALIGN: usize = 16;

    fn layout(size: usize, align: usize) -> Option<Layout> {
        Layout::from_size_align(align.checked_add(size)?, align).ok()
    }

    unsafe fn header(ptr: *mut u8) -> (usize, usize) {
        unsafe { (*(ptr.sub(16) as *const usize), *(ptr.sub(8) as *const usize)) }
    }

    /// `size` bytes aligned to `align`, a power of two.
    pub unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
        let align = align.max(MIN_ALIGN);
        let Some(layout) = layout(size, align) else { return ptr::null_mut() };
        let raw = unsafe { alloc::alloc::alloc(layout) };
        if raw.is_null() {
            return raw;
        }
        unsafe {
            let ptr = raw.add(align);
            *(ptr.sub(16) as *mut usize) = size;
            *(ptr.sub(8) as *mut usize) = align;
            ptr
        }
    }

    pub unsafe fn dealloc(ptr: *mut u8) {
        let (size, align) = unsafe { header(ptr) };
        let layout = layout(size, align).expect("a block's header is the layout it was allocated with");
        unsafe { alloc::alloc::dealloc(ptr.sub(align), layout) };
    }

    pub unsafe fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
        let (size, align) = unsafe { header(ptr) };
        if align > MIN_ALIGN {
            let new = unsafe { alloc(new_size, align) };
            if !new.is_null() {
                unsafe {
                    ptr::copy_nonoverlapping(ptr, new, size.min(new_size));
                    dealloc(ptr);
                }
            }
            return new;
        }
        let Some(new_total) = MIN_ALIGN.checked_add(new_size) else { return ptr::null_mut() };
        let layout = layout(size, MIN_ALIGN).expect("a block's header is the layout it was allocated with");
        let raw = unsafe { alloc::alloc::realloc(ptr.sub(MIN_ALIGN), layout, new_total) };
        if raw.is_null() {
            return raw;
        }
        unsafe {
            let ptr = raw.add(MIN_ALIGN);
            *(ptr.sub(16) as *mut usize) = new_size;
            *(ptr.sub(8) as *mut usize) = MIN_ALIGN;
            ptr
        }
    }
}

// --- C standard memory functions ---

#[cfg(not(feature = "std-runtime"))]
#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    if size == 0 {
        return ptr::null_mut();
    }
    unsafe { backend::alloc(size, 16) }
}

#[cfg(not(feature = "std-runtime"))]
#[no_mangle]
pub unsafe extern "C" fn aligned_alloc(align: usize, size: usize) -> *mut u8 {
    if !align.is_power_of_two() {
        crate::errno::set(crate::errno::EINVAL);
        return ptr::null_mut();
    }
    let p = unsafe { backend::alloc(size, align) };
    if p.is_null() {
        crate::errno::set(crate::errno::ENOMEM);
    }
    p
}

#[cfg(not(feature = "std-runtime"))]
#[no_mangle]
pub unsafe extern "C" fn posix_memalign(out: *mut *mut u8, align: usize, size: usize) -> i32 {
    if !align.is_power_of_two() || align % core::mem::size_of::<usize>() != 0 {
        return crate::errno::EINVAL;
    }
    let p = unsafe { backend::alloc(size, align) };
    if p.is_null() {
        return crate::errno::ENOMEM;
    }
    unsafe { *out = p };
    0
}

#[cfg(not(feature = "std-runtime"))]
#[no_mangle]
pub unsafe extern "C" fn free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    unsafe { backend::dealloc(p); }
}

#[cfg(not(feature = "std-runtime"))]
#[no_mangle]
pub unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut u8 {
    let total = match count.checked_mul(size) {
        Some(t) => t,
        None => return ptr::null_mut(),
    };
    let p = unsafe { malloc(total) };
    if !p.is_null() {
        unsafe { ptr::write_bytes(p, 0, total); }
    }
    p
}

#[cfg(not(feature = "std-runtime"))]
#[no_mangle]
pub unsafe extern "C" fn realloc(p: *mut u8, new_size: usize) -> *mut u8 {
    if p.is_null() {
        return unsafe { malloc(new_size) };
    }
    if new_size == 0 {
        unsafe { free(p); }
        return ptr::null_mut();
    }
    unsafe { backend::realloc(p, new_size) }
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
