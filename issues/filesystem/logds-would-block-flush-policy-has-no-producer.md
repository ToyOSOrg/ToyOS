---
status: open
kind: finding
opened: 2026-09-27
---

# logd's would-block flush policy has no producer

`userland/logd/src/policy.rs`'s `fate` retries a flush answered
`io::ErrorKind::WouldBlock` for up to `LOG_WRITE_BUDGET`, and its module doc and
`userland/logd/src/main.rs`'s header argue why. logd's `/log` is served by fsd,
whose `word` (`userland/fsd/src/fat.rs`) answers no refusal `WouldBlock`, and
`toyos-fat32` has no budget refusal left to carry one, so the arm, its tests and
both docs describe an answer nothing sends.

## Exit condition

The `(Step::Flush, WouldBlock)` arm, its tests and the prose arguing for it are
deleted, or fsd answers a refusal `WouldBlock` and a guest test shows logd
retrying it.
