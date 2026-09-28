---
status: open
kind: defect
opened: 2026-09-27
---

# Six kernel actuators lost the only test that armed them

The test schedule deleted the guest tests that never caught a defect, and six
actuators in `kernel/src/actuator.rs` were armed by nothing else: no row of
`tests/toyos.rs`, `tests/common/` or `tests/metal-profile.toml` names them now.

| actuator | the deleted test | the kernel site it guards |
|---|---|---|
| `klogd-panic` | `klogd_panic_halts` | `kernel/src/log/console.rs` |
| `quiesce-dump` | `quiesce_dump_holds_the_stopped` | `kernel/src/sched/dump.rs` |
| `quiesce-last-exit` | `quiesce_wakes_on_the_last_exit` | `kernel/src/quiesce.rs`, `toyos-quiesce/src/lib.rs` |
| `so-cache-tiny` | `so_cache_refusals` | `kernel/src/elf/cache.rs` |
| `xhci-hid-break-first` | `xhci_hid_break` | the xHCI HID completion path |
| `xhci-hid-break-late` | `xhci_hid_break` | the xHCI HID completion path |

An actuator nothing arms is kernel code no run executes: the `test-actuators`
kernel still compiles each arm, and no verdict reads what it stages.

Found by: `git grep` for each actuator's name over `tests/` and `src/` after the
deletions on `wt/toyos-schedule`, every count 0.

**Exit**: each actuator and the code only it reaches deleted from the kernel,
or a test that arms it registered again. Owner: orchestrator.
