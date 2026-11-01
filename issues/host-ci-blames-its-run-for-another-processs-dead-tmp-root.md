---
status: open
kind: tooling
opened: 2026-11-01
---

# Host CI blames its run for another process's dead /tmp root

`cargo run -- --ci host`'s last step, "nothing left in $TMPDIR or /tmp", turns red for any `toyos_tmpdir` root under the short directory, `/tmp`, that appeared during the run and whose process has since died (`left_behind`, `build/ci.rs`). It does not ask whether that process was one of the run's own. Several agents share one host's `/tmp`, so a guest test elsewhere that dies mid-boot reddens an unrelated host run.

Observed 2026-11-01. A host run of an issue-only change, on branch `display-no-psr`, went red on `/tmp/toyos-tmp-16566-0/boot-0`. That is a QEMU boot's short QMP directory (`build/qemu.rs`). The root was made at 22:06, while the run was in `system/fileserver`'s unit tests. No step of a host run boots QEMU, and the run's own scratch is under `$TMPDIR`. The other 107 steps were green.

**Exit**: the step counts only roots made by the run's own process tree, for example roots whose recorded owner pid descends from the run. A test shows a dead root of an unrelated process left unblamed.

Owner: whoever holds the CI driver (`build/ci.rs`).
