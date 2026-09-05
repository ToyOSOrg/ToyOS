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

**`SYS_FTRUNCATE` is not the only one, and one of them has been seen red.**
`kernel/src/fat32_adapter.rs:546` maps `Error::BudgetExpired` to `WouldBlock`
for every FAT operation, so a *delete* carries the same status out with nothing
behind it. #377's adversarial review recorded `fs_transactional` red once in its
runs on the dev host — `ALONE` pass, 0 of 6 further runs, not on
`src/redlist.rs`'s list, all twelve CI shards green:

    usb-storage: ... transport broke on SCSI 0x2a: no answer in the status phase in 2000 ms
    log-volume: delete of fstx_keep_dst.bin: the device would not answer in the caller's own budget

Seen a second time on the dev host, on a `kernel-userland-reach` fast tier of
103 guests with four sibling worktrees running beside it: the same two lines,
`cleanup: Kind(WouldBlock)` at `fs_transactional.rs:48`, `ALONE` pass again.
Two sightings, both beside other guests, and still no rate.

That is the two-deadlines-in-series producer `src/redlist.rs:2994` retired for
`esp_filesystem` on 2026-08-23 — `USB_TIMEOUT_NS` breached on a WRITE(10) status
phase, then `block::OPERATION` refusing the retry unissued. The retry that
retired it lives in `ops::fsync` and covers `SYS_FSYNC` alone, so the class it
closed still reaches userland on every other path. Whoever fixes this fixes
both, which is the other half of why the answer is where retries live.

**Exit condition.** Either the budget expiry is retried above the lock the way
`SYS_FSYNC` does — for the truncate and the delete both — and only what survives
the deadman is reported; or the mapping stops claiming retryability and the
reason is written at the site.
Whichever, the `resize-fault-refuse` actuator already stages the refusal, so the
chosen answer has an instrument to be measured with.
