---
status: open
kind: defect
opened: 2026-09-02
---

# `SYS_FTRUNCATE` can answer `WouldBlock` and no caller in the tree retries it

A shrink whose new end falls inside a page the cache does not hold reads that
page off the device first (`file_cache::resize`). That read can be refused on the
caller's own time budget — `block::begin_operation` is armed per transfer by
`kernel/src/drivers/nvme.rs` and `kernel/src/drivers/usb_storage.rs`, and a
refusal there is `BlockError::BudgetExpired`. `ops::ftruncate` maps it to
`SyscallError::WouldBlock`, which is the truth: nothing was resized and asking
again on a fresh budget would work.

Nothing asks again. `userland/libc/src/posix_io.rs`'s `ftruncate` is

    match syscall::ftruncate(fd(raw_fd), length as u64) {
        Ok(()) => 0,
        Err(e) => set_errno(e),
    }

so the status reaches a C caller as `EAGAIN` from a call POSIX gives no
`EAGAIN`, and Rust's `File::set_len` surfaces it as `ErrorKind::WouldBlock`. The
comparison is `SYS_FSYNC`, which has the retry ladder this does not
(`ops::fsync`'s loop over `block::DEADMAN`): there, a budget expiry is retried
above every lock until the deadman, and only then reported.

Not a loss either way — a refused resize changes nothing — so this is a spurious
failure, not corruption. It is filed rather than fixed here because the fix is a
retry ladder in a second syscall, which is a decision about where retries live
rather than a patch.

**Exit condition.** Either `SYS_FTRUNCATE` retries a budget expiry above its
lock the way `SYS_FSYNC` does, and reports only what survives the deadman; or
the mapping stops claiming retryability and the reason is written at the site.
Whichever, the `resize-fault-refuse` actuator already stages the refusal, so the
chosen answer has an instrument to be measured with.
