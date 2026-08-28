---
status: open
kind: defect
opened: 2026-08-28
---

# The bcachefs adapter turns a retryable `BudgetExpired` into `Io`, and the write-back queue drops the file's pages on it

`kernel/CLAUDE.md:16` states the rule: *"A block-layer `BudgetExpired` is
not-durable-yet and never a loss — it is retried on a fresh budget above every
lock."* The `/home` filesystem does not keep it.

## The mechanism

`BlockError` has exactly two variants and the second one is the retry signal:
`Device` is the device's own word, `BudgetExpired` is "refused before it was
attempted because the operation's time budget expired; safe to retry"
(`kernel/src/block.rs:77-82`). `block::OPERATION` (2 s, `block.rs:9-14`) bounds
one operation and says "the caller's own give-up policy decides whether to ask
again"; `block::DEADMAN` (120 s, `block.rs:16-21`) is where giving up is legal.

Every layer under the bcachefs adapter preserves that distinction on purpose.
`NvmeController::unanswered` (`kernel/src/drivers/nvme.rs:342-346`) returns
`BudgetExpired` for a command the controller never answered *and the reset
escalation reclaimed* — the same escalation logs "the disk stays online and the
caller is told to ask again" (`nvme.rs:324-326`). `may_issue`
(`nvme.rs:411-419`) returns it for a spent budget, "never a fact about the
controller". `PageCache::sync` combines failures with `BlockError::worse`
rather than first-wins, with the reason written down: "so a caller sees the one
that blocks retry" (`kernel/src/page_cache.rs:258`).

The bcachefs adapter is that caller, and it discards the value at four sites:

- `BcacheFsAdapter::write_page` — `page_cache::raw_block_write(block, data)
  .map_err(|_| { log!(...); SyscallError::Io })`
  (`kernel/src/bcachefs_adapter.rs:246-249`). The `BlockError` is in hand and
  thrown away with `|_|`.
- `PageCacheBlockIO::read_block` / `write_block` / `sync` — `.map_err(|_|
  DeviceError)` (`bcachefs_adapter.rs:22`, `:30`, `:43`), under a header
  comment that claims the opposite: "Errors propagate unchanged; nothing here
  invents a value for a refused transfer" (`bcachefs_adapter.rs:17`).
  `bcachefs`'s `DeviceError` is a unit struct (`bcachefs/src/block_io.rs:64`),
  so this collapse is structural, not just a lost match arm; `BlockIOExt`
  widens it to `FsError::DeviceRead/DeviceWrite/DeviceSync`
  (`block_io.rs:121-129`) and `as_syscall_error` maps all three to
  `SyscallError::Io` (`bcachefs_adapter.rs:53`).

No bcachefs path can therefore ever produce `SyscallError::WouldBlock` — the
word every retry loop in the kernel keys on. Grepping `WouldBlock` across
`bcachefs_adapter.rs`, `vfs.rs` and `page_cache.rs` returns nothing, and
`BlockError::BudgetExpired` is matched outside the drivers and gates in exactly
one place in the tree: `kernel/src/fat32_adapter.rs:104`.

## The chain, on current main

1. `writeback::drain_one` pops a closed file's deferred teardown
   (`kernel/src/writeback.rs:104-110`) and flushes it
   (`writeback.rs:117`).
2. `Vfs::flush_file` → `flush_taken` writes each dirty page with
   `fs.write_page(file_id, page_idx, buf)?` (`kernel/src/vfs.rs:370`) and then
   `fs.update_metadata(...)?` (`vfs.rs:376`).
3. `BcacheFsAdapter::write_page` returns `SyscallError::Io` for a
   `BudgetExpired` device refusal (`bcachefs_adapter.rs:246-249`);
   `update_metadata` returns `Io` too, through
   `mapped` → `as_syscall_error` (`bcachefs_adapter.rs:53`, `:65-70`).
4. `drain_one` re-enqueues **only** on `Err(SyscallError::WouldBlock)` within
   the deadman (`writeback.rs:121-125`). An `Io` falls to `writeback.rs:127-132`:
   `log!("writeback: {} is not durable ({e:?}); its unflushed pages are lost")`,
   then falls through.
5. `file_cache::finish_writeback` (`writeback.rs:137`) drops the file when
   `file.deleted || file.evictable` (`kernel/src/file_cache.rs:185-186` →
   `drop_file`, `file_cache.rs:447-452`). Every bcachefs `/home` file is created
   evictable (`bcachefs_adapter.rs:161`, `:182`), so this is always taken.

The pages were recoverable right up to step 5: `take_dirty` only reads which
pages are dirty (`file_cache.rs:338-343`), `clear_dirty` never ran, and
`flush_file` restored `dirty_meta` (`vfs.rs:345-347`) — the retry the file was
owed would have delivered the same bytes. `drop_file` removes the file entry
and its pages instead.

`SYS_FSYNC` loses nothing but lies in the same way: its loop retries
`WouldBlock` to `block::DEADMAN` (`kernel/src/object/ops.rs:538-554`) and
returns any other error straight through (`ops.rs:555`), so an `fsync` on
`/home` reports `Io` — the device's own word — after a single 2 s hiccup on a
controller that is alive and was asking to be asked again, spending none of the
120 s the ladder exists to spend.

## Impact

Silent, permanent loss of a user's written bytes on the default read-write
filesystem, from a transient the block layer classified as retryable. The
syscall caller is already gone — the write-back queue runs after the last
close — so the only trace is one line into `/log`, a userland file the user may
never read. The machine stays up and reports nothing.

## Precondition

`/home` mounted read-write with `BcacheFsAdapter` (`kernel/src/main.rs:421`,
taken whenever `nvme::init` yielded a home volume, `main.rs:351`), an
unprivileged process writing an ordinary file there and closing it, and any one
NVMe command in the resulting flush going unanswered for `COMMAND` = 2 s
(`nvme.rs:41-44`, `nvme.rs:169-178`) with the controller reset then succeeding
(`nvme.rs:312-327`). No hostile device is required — a stalled or contended
controller is enough, and the same family has been observed for real on the USB
side (`issues/build/fat-backing-revoked-panics-on-a-budget-refused-create.md`
records a live "ran out of its operation budget" under host load). Note that a
`write_page` is one block and a `PageCache::sync` batch is at most
`MAX_DATA_PAGES` = 32 (`nvme.rs:244`), each one command under a budget
`write_blocks` takes fresh (`nvme.rs:660`), so `may_issue`'s pre-check is not
the live route on these paths — the reset-reclaimed silence is.

## Why no test catches it

The whole slow-vs-failed policy is staged against `/log`, a FAT32 volume:
`tests/common/volumes.rs:1943-1969` boots with `fsync-budget-spent` and asserts
the retry, and `:2062-2072` asserts the deadman. `fsync-budget-spent` works by
establishing an outer already-passed `Operation` around the first attempt
(`ops.rs:512-514`), which `Operation::begin` mins into every nested block
operation (`kernel/src/scheduler.rs:132-148`) — so the actuator to stage this
on `/home` already exists; nothing points it there.

## Fix direction

Thread the discriminant instead of erasing it, and price it with a negative
control on the `/home` path.

- The kernel-side half is local: `write_page` has the `BlockError` in hand, so
  map `BudgetExpired → SyscallError::WouldBlock` and `Device → SyscallError::Io`
  (`bcachefs_adapter.rs:246-249`), exactly as `fat32_adapter.rs:100-105` and
  `:511-513` do. `update_metadata` must move with it, or a fixed `write_page`
  still loses the file at `vfs.rs:376`.
- The crate-side half needs `bcachefs`'s `DeviceError` (`bcachefs/src/block_io.rs:64`)
  to carry a retryable/failed discriminant, propagated through
  `FsError::DeviceRead/DeviceWrite/DeviceSync` (`block_io.rs:121-129`) so
  `as_syscall_error` (`bcachefs_adapter.rs:53`) can split them. Until that lands,
  metadata and `sync` refusals stay collapsed and the fix is partial — say so
  rather than closing this.
- Prefer making the erasure unrepresentable over adding a match: a `|_|` on a
  two-variant error whose variants mean opposite give-up policies is the defect,
  and a `From<BlockError>` at the one boundary that must convert would have
  failed to compile here.
- The control: point `fsync-budget-spent` at a `/home` file and assert the same
  two things the `/log` arm asserts — a `fsync: … durable on attempt N` line,
  and the file's bytes byte-identical off the image afterwards. Reverting the
  whole change must red it.
