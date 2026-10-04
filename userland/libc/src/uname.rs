//! `uname`, answered from the build ROOT carries (`toyos-osrelease`): `ToyOS`,
//! no node name — no host name is published to a process — the commit's
//! twelve digits as the release, `-dirty` after them for a tree that was not
//! that commit's, the whole commit as the version, and the machine.

use toyos_abi::syscall::{self, OpenFlags};

use crate::errno::{self, EIO};

#[repr(C)]
pub struct Utsname {
    sysname: [u8; 65],
    nodename: [u8; 65],
    release: [u8; 65],
    version: [u8; 65],
    machine: [u8; 65],
}

/// Room for any file the build writes, and a file this fills is not one.
const FILE_BYTES: usize = 512;

#[no_mangle]
pub unsafe extern "C" fn uname(buf: *mut Utsname) -> i32 {
    let mut file = [0u8; FILE_BYTES];
    let len = match read(toyos_osrelease::GUEST_PATH, &mut file) {
        Ok(len) => len,
        Err(e) => return crate::posix_io::set_errno(e),
    };
    let Ok(release) = toyos_osrelease::parse(&file[..len]) else {
        errno::set(EIO);
        return -1;
    };
    let out = &mut *buf;
    put(&mut out.sysname, &[toyos_osrelease::NAME]);
    put(&mut out.nodename, &[]);
    let dirty = match release.tree {
        toyos_osrelease::Tree::Clean => "",
        toyos_osrelease::Tree::Dirty => "-dirty",
    };
    put(&mut out.release, &[release.short(), dirty]);
    put(&mut out.version, &[release.commit.as_str()]);
    put(&mut out.machine, &[release.arch.machine()]);
    0
}

/// `parts`, one after another and NUL-terminated, into `field`.
fn put(field: &mut [u8; 65], parts: &[&str]) {
    let mut at = 0;
    for part in parts {
        field[at..at + part.len()].copy_from_slice(part.as_bytes());
        at += part.len();
    }
    field[at..].fill(0);
}

/// As much of the file at `path` as `into` holds.
fn read(path: &str, into: &mut [u8]) -> Result<usize, syscall::SyscallError> {
    let handle = syscall::open(path.as_bytes(), OpenFlags::READ)?;
    let mut len = 0;
    let read = loop {
        match syscall::read(handle, &mut into[len..]) {
            Ok(0) => break Ok(len),
            Ok(n) => {
                len += n;
                if len == into.len() {
                    break Ok(len);
                }
            }
            Err(e) => break Err(e),
        }
    };
    syscall::close(handle);
    read
}
