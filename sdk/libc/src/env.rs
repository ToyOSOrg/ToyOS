//! The environment: `environ`, `getenv`, `setenv` and `unsetenv`, read from
//! the block the kernel started this process with.
//!
//! **A string `setenv` replaces or `unsetenv` removes is never freed**: a
//! program that keeps what `getenv` answered across a later call, which POSIX
//! lets that call invalidate, reads it intact rather than freed. A program
//! that sets one variable again and again grows its memory by each value.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;

use crate::pthread::Lock;

/// The environment, the strings `environ` points at, each `KEY=VALUE\0`.
struct Env {
    strings: Vec<*mut u8>,
    /// `strings` and a null, which `environ` points at.
    array: Vec<*mut u8>,
}

// SAFETY: the pointers are this module's own strings, read and changed under the lock.
unsafe impl Send for Env {}

static ENV: Lock<Option<Env>> = Lock::new(None);

#[no_mangle]
pub static mut environ: *mut *mut u8 = ptr::null_mut();

/// `entry` as a string of its own, leaked: see the module's note.
fn leak(entry: &[u8]) -> *mut u8 {
    let mut owned = Vec::with_capacity(entry.len() + 1);
    owned.extend_from_slice(entry);
    owned.push(0);
    Box::leak(owned.into_boxed_slice()).as_mut_ptr()
}

/// The `KEY=VALUE` a string `strings` holds, without its `NUL`.
///
/// # Safety
/// `s` is one of [`Env::strings`].
unsafe fn entry<'a>(s: *mut u8) -> &'a [u8] {
    crate::posix_io::c_str_to_bytes(s)
}

/// Whether `entry` is `name`'s.
fn is(entry: &[u8], name: &[u8]) -> bool {
    entry.len() > name.len() && entry[name.len()] == b'=' && entry.starts_with(name)
}

impl Env {
    fn read() -> Env {
        let mut block = Vec::new();
        loop {
            let need = toyos_abi::syscall::get_env(&mut block);
            if need <= block.len() {
                block.truncate(need);
                break;
            }
            block.resize(need, 0);
        }
        let strings: Vec<*mut u8> = block.split(|&b| b == 0).filter(|e| !e.is_empty()).map(leak).collect();
        let mut env = Env { strings, array: Vec::new() };
        env.publish();
        env
    }

    /// Point `environ` at `strings`, ended by a null.
    fn publish(&mut self) {
        self.array.clear();
        self.array.extend_from_slice(&self.strings);
        self.array.push(ptr::null_mut());
        // SAFETY: written under the lock; a reader of `environ` racing a
        // `setenv` is a race POSIX leaves undefined.
        unsafe { environ = self.array.as_mut_ptr() };
    }

    fn position(&self, name: &[u8]) -> Option<usize> {
        // SAFETY: every one is a string of `strings`.
        self.strings.iter().position(|&s| is(unsafe { entry(s) }, name))
    }
}

/// The environment, read the first time it is asked for.
fn with<T>(f: impl FnOnce(&mut Env) -> T) -> T {
    let mut env = ENV.lock();
    f(env.get_or_insert_with(Env::read))
}

/// Make `environ` before a C program's `main` reads it.
#[cfg(not(feature = "std-runtime"))]
pub(crate) fn init() {
    with(|_| ());
}

/// A name `setenv` and `unsetenv` take: not empty, and no `=` in it.
fn name_of(name: *const u8) -> Option<&'static [u8]> {
    if name.is_null() {
        return None;
    }
    let name = crate::posix_io::c_str_to_bytes(name);
    (!name.is_empty() && !name.contains(&b'=')).then_some(name)
}

#[no_mangle]
pub unsafe extern "C" fn getenv(name: *const u8) -> *mut u8 {
    let Some(name) = name_of(name) else { return ptr::null_mut() };
    with(|env| match env.position(name) {
        // SAFETY: the value follows `name=` in that string.
        Some(at) => unsafe { env.strings[at].add(name.len() + 1) },
        None => ptr::null_mut(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn setenv(name: *const u8, value: *const u8, overwrite: i32) -> i32 {
    let Some(name) = name_of(name) else {
        crate::errno::set(crate::errcode::EINVAL);
        return -1;
    };
    let value = crate::posix_io::c_str_to_bytes(value);
    with(|env| {
        let at = env.position(name);
        if at.is_some() && overwrite == 0 {
            return;
        }
        let mut line = Vec::with_capacity(name.len() + 1 + value.len());
        line.extend_from_slice(name);
        line.push(b'=');
        line.extend_from_slice(value);
        match at {
            Some(at) => env.strings[at] = leak(&line),
            None => env.strings.push(leak(&line)),
        }
        env.publish();
    });
    0
}

#[no_mangle]
pub unsafe extern "C" fn unsetenv(name: *const u8) -> i32 {
    let Some(name) = name_of(name) else {
        crate::errno::set(crate::errcode::EINVAL);
        return -1;
    };
    with(|env| {
        // SAFETY: every one is a string of `strings`.
        env.strings.retain(|&s| !is(unsafe { entry(s) }, name));
        env.publish();
    });
    0
}

/// The environment as `SYS_SPAWN` takes one: each `KEY=VALUE`, ended by `NUL`.
pub(crate) fn blob() -> Vec<u8> {
    with(|env| {
        let mut out = Vec::new();
        for &s in &env.strings {
            // SAFETY: every one is a string of `strings`.
            out.extend_from_slice(unsafe { entry(s) });
            out.push(0);
        }
        out
    })
}

/// The first file named `name` under a directory of `PATH`, with its `NUL`:
/// `/system/bin` where there is no `PATH`.
pub(crate) fn find_in_path(name: &[u8]) -> Option<Vec<u8>> {
    let path: Vec<u8> = match unsafe { getenv(b"PATH\0".as_ptr()) } {
        p if p.is_null() => b"/system/bin".to_vec(),
        p => crate::posix_io::c_str_to_bytes(p).to_vec(),
    };
    path.split(|&b| b == b':').filter(|dir| !dir.is_empty()).find_map(|dir| {
        let mut candidate = Vec::with_capacity(dir.len() + 1 + name.len() + 1);
        candidate.extend_from_slice(dir);
        candidate.push(b'/');
        candidate.extend_from_slice(name);
        candidate.push(0);
        crate::posix_io::is_file(&candidate[..candidate.len() - 1]).then_some(candidate)
    })
}
