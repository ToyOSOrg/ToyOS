//! The revoked-backing control, behind `revoked-backing-selftest`: on the
//! kernel's one writable mount, `/tmp`, a `FileBacking` read after the file's
//! deletion must fail rather than fault in zeros.

use crate::file_cache;
use crate::mm::PAGE_BYTES;
use crate::vfs;

const FILL: u8 = 0xA7;

pub fn run() {
    probe("/tmp/revoke_probe");
}

/// FAIL names the step so the verdict line carries the mechanism, not just the arm.
fn fail(path: &str, step: &str) {
    crate::log!("revoke-selftest: {path} FAIL ({step})");
}

fn probe(path: &str) {
    let mtime = crate::clock::nanos_since_boot();
    let id = match vfs::lock().create_file(path, mtime) {
        Ok(id) => id,
        Err(e) => return fail(path, &alloc::format!("create: {e:?}")),
    };
    if file_cache::write_page(id, 0, 0, &[FILL; 64][..]).is_err() {
        return fail(path, "write");
    }
    // Released the way `OpenFileState::drop` does.
    file_cache::release(id);
    let backing = match vfs::lock().open_backing(path) {
        Ok(b) => b,
        Err(e) => return fail(path, &alloc::format!("open_backing: {e:?}")),
    };

    let mut heap = alloc::vec![0u8; PAGE_BYTES].into_boxed_slice();
    let buf: &mut [u8; PAGE_BYTES] = (&mut heap[..]).try_into().expect("PAGE_BYTES bytes");
    match backing.read_page(0, buf) {
        Ok(()) if buf[0] == FILL => {}
        Ok(()) => return fail(path, &alloc::format!("pre-delete read served {:#04x}", buf[0])),
        Err(_) => return fail(path, "pre-delete read refused"),
    }

    if let Err(e) = vfs::lock().delete_file(path) {
        return fail(path, &alloc::format!("delete: {e:?}"));
    }

    match backing.read_page(0, buf) {
        Err(_) => crate::log!("revoke-selftest: {path} PASS (pre={FILL:#04x}, post refused)"),
        Ok(()) => fail(
            path,
            &alloc::format!("post-delete read succeeded, byte 0 = {:#04x}", buf[0]),
        ),
    }
}
