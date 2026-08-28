//! Negative controls for the leak-rollback fixes: each reproduces an "acquire
//! before a fallible step" site and asserts the in-tree count returns to baseline. Run behind `leak-rollback-selftest`.

/// Runs every leak-rollback control; must run after `/log` is mounted.
pub fn run() {
    crate::object::device::mint_rollback_selftest();
    fat_reopen_census();
}

/// The reopen arm takes the file-cache reference before the fallible backing
/// lookup; a transient device error there must leave the reference count unchanged.
fn fat_reopen_census() {
    use crate::vfs;

    const PATH: &str = "/log/lrfile";

    let mtime = crate::clock::nanos_since_boot();
    let id = match vfs::lock().create_file(PATH, mtime) {
        Ok(id) => id,
        Err(e) => {
            crate::log!("leak-selftest: fat-reopen skipped, create failed: {e:?}");
            return;
        }
    };

    let before = crate::file_cache::ref_count(id);
    crate::fat32_adapter::selftest_fail_backing(true);
    // Each `vfs::lock()` is its own statement: the guard is a re-entrant spinlock.
    let target = vfs::lock().resolve_for_open(PATH, vfs::ResolveIntent::KernelOrRead);
    let reopened = match target {
        Ok(target) => vfs::lock().open_target(&target),
        Err(e) => Err(e),
    };
    crate::fat32_adapter::selftest_fail_backing(false);
    let after = crate::file_cache::ref_count(id);

    let verdict = if reopened.is_err() && after == before { "PASS" } else { "FAIL" };
    crate::log!(
        "leak-selftest: fat-reopen {verdict} (refused={} before={before} after={after})",
        reopened.is_err()
    );
}
