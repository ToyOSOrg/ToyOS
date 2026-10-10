---
status: open
kind: tooling
opened: 2026-10-10
---

# The host gate aborts with no message when a destructor panics during unwinding

`cargo run -- --ci host > <file> 2>&1` at `d242c7f05` (branch `wt/toyos-amdvi`)
exited 134. Its log stops mid-step, at the `failures:` header of the
`mutate-landed-child-retires-nothing` control, with no panic message and no
verdict. The rerun at the same head, nothing changed, exited 0.

macOS crash reports show five processes aborting within 0.7 s of each other,
from three different binaries, each with
`SIGABRT` from `abort()`. In every one the faulting thread is
`core::panicking::panic_in_cleanup`, a destructor panicking while another
panic unwound, directly under:

- `toyos_build::ci::host`, called from `ci::dispatch` and `main`: three
  `toyos-build` processes, two of them one binary.
- `toyos_build::build::build_test_parts`, under `build_test_image`,
  `common::qemu::build_boot_image_with`, `QemuInstance::boot_with_options`
  and `virt_selftest`, on a `run_tasks` worker: one guest-suite process.
- `common::lane::Run::exit`, called from the guest suite's `main`: one
  guest-suite process.

Right after the abort the data volume holding every worktree, its `target/`
and the gate's redirected log was 99% full: 926 GiB, 887 GiB used, 12 GiB
available. Eight minutes later it read 82%, 169 GiB available.

What reading shows, not yet measured: every one of those frames reaches a
`Drop` that writes to stderr (`build::Building`, `lane::Run`), and
`eprintln!` panics when its write fails. A full volume under a redirected
stderr is one way for all five to fail inside the same second. The trigger is
unconfirmed; the system log for that minute was not readable.

Whatever the trigger, a gate that dies without saying why is a defect of its
own: a destructor reached while unwinding must not panic.

**Exit:** a host test in which `Building`'s and `Run`'s destructors run during
an unwinding panic with stderr refusing every write, and the process exits non-zero by
its own exit code, never by `SIGABRT`.
