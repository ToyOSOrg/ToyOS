pub use super::common::Env;
use crate::collections::HashMap;
use crate::ffi::{OsStr, OsString};
use crate::io;
use crate::sync::{Mutex, MutexGuard};

static ENV: Mutex<Option<HashMap<OsString, OsString>>> = Mutex::new(None);

/// Lock ENV and lazily initialize from the kernel if needed.
/// This ensures env works from cdylib .so files whose std was never explicitly initialized.
fn lock_env() -> MutexGuard<'static, Option<HashMap<OsString, OsString>>> {
    let mut guard = ENV.lock().unwrap();
    if guard.is_none() {
        let mut map = HashMap::new();
        let size = toyos_abi::syscall::get_env(&mut []);
        if size > 0 {
            let mut buf = vec![0u8; size];
            toyos_abi::syscall::get_env(&mut buf);
            for entry in buf.split(|&b| b == 0) {
                if entry.is_empty() {
                    continue;
                }
                if let Some(eq) = entry.iter().position(|&b| b == b'=') {
                    let key = OsString::from(crate::str::from_utf8(&entry[..eq]).unwrap_or(""));
                    let val = OsString::from(crate::str::from_utf8(&entry[eq + 1..]).unwrap_or(""));
                    map.insert(key, val);
                }
            }
        }
        *guard = Some(map);
    }
    guard
}

pub fn init() {
    drop(lock_env());
}

pub fn env() -> Env {
    let guard = lock_env();
    let map = guard.as_ref().unwrap();
    let result = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    Env::new(result)
}

pub fn getenv(k: &OsStr) -> Option<OsString> {
    lock_env().as_ref().unwrap().get(k).cloned()
}

pub unsafe fn setenv(k: &OsStr, v: &OsStr) -> io::Result<()> {
    lock_env().as_mut().unwrap().insert(k.to_owned(), v.to_owned());
    Ok(())
}

pub unsafe fn unsetenv(k: &OsStr) -> io::Result<()> {
    lock_env().as_mut().unwrap().remove(k);
    Ok(())
}
