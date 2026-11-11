---
status: open
kind: tooling
opened: 2026-11-11
---

# process_tree's metal judge waits for a kernel line no spawn prints

The metal `process_tree` row's judge (`process_tree` in `tests/toyos.rs`)
requires the kernel's log to say `spawn: /tmp/process_tree_invalid_elf: `
twice, once per B: the kernel loader refusing the 64 zero bytes the job
writes there. Since the spawner lays a program out itself
(`toyos::elf::program::lay_out` in `Helper::spawn`, `sdk/toyos/src/spawn.rs`),
an image that is no ELF is refused in the caller before any syscall, so the
kernel never sees it and never prints the line. The job still passes: B
asserts only that the spawn failed.

Evidence: the T14's `proctreecase` boot of #1203 at e78085e47. The job ended
`===TEST_END test_rs_process_tree exit=0===`, the kernel's log holds no line
naming `/tmp/process_tree_invalid_elf`, and the judge answered `the loader
refused the invalid executable 0 times, not once per B`; the row's other
three tests passed. #1203 changes no kernel, spawn or `process_tree` code.
https://github.com/ToyOSOrg/evidence/tree/eff6d9e767dc90c4472908660613872f68f76dbb/r3.

So the row is red on every bench, and what it meant to hold, a spawn refused
after admission under B releasing what admission took, is no longer reached
by this job.

Exit: the job reaches a kernel refusal after admission again, with an image
that lays out and the kernel refuses, and the judge counts that line; or the
arm and the judge's count go, if a cheaper test holds that release.

Owner: the guest-suite track.
