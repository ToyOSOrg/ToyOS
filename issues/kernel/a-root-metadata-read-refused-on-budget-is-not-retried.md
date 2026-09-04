---
status: open
kind: defect
opened: 2026-09-04
---

# A ROOT metadata read refused on the caller's budget is not retried

Every file on ROOT is opened by walking a btree, and every btree block reaches
the kernel through `bcachefs_adapter::PageCacheBlockIO::read_block` — which
locks the cache, then the device, then transfers. A transfer refused on
`block::OPERATION` comes back `BlockError::BudgetExpired`, and that becomes
`DeviceError::Refused`, `FsError::DeviceRead`, `SyscallError::WouldBlock`.
Nothing retries it: a `spawn` whose `file_extents` lookup lands on a refused
btree read reports the program missing on a disk that is merely busy.

`kernel/CLAUDE.md`'s rule is that a `BudgetExpired` is a claim about the
caller's clock and never a loss, retried on a fresh budget above every lock.
The retry cannot go where the read is: `read_block` runs under the cache lock
and the retry has to be above it. `file_backing::read_block_retrying` is the
same rule applied to the *data* path, which is where it was reached first —
twelve guests sharing one host spent the budget waiting for the xHCI
controller lock and demand-paged executables faulted at `_start+0x0`.

**Reproduction.** Not reached in a suite yet: the data path was, and this one
shares its mechanism and its device. `usb-slow-device` holds every mass-storage
completion back, and a boot armed with it that also spawns under load is the
shape to try.

**Exit condition.** A refused metadata read is retried on a fresh budget above
the cache lock, bounded by `block::DEADMAN`, or the mount reports it as the
retryable refusal it is and every caller of `open` acts on that word.
