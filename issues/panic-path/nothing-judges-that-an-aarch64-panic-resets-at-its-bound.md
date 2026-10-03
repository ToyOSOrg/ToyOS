---
status: open
kind: tooling
opened: 2026-10-02
---

# Nothing judges that an AArch64 panic resets the machine at its bound

Since PSCI gave AArch64 a reset, a panic there after `psci::init` arms
`panic_reboot`'s bound, where it used to hold the panel, and at the bound's
end `reboot_now` calls `SYSTEM_RESET`. No test waits for that reset.
`virt_fatal_halts_the_others_first` holds the arm line, `panic: rebooting in
60 s, timed by`, and ends there; `virt_reboot` judges `SYSTEM_RESET` from the
syscall, not from a panic.

A test is one boot and costs the shipped bound, `toyos_tco::PANIC_BOUND_MS`,
60 s of every suite run, on a suite that takes 45 s. The actuator that
shortened the bound for a judge, `panic-reboot-fast`, went with the x86-64
tests that used it, and AArch64 has no metal row to move this to.

**Evidence**: the measurement, once, at `94dea677c`, as a test added by a
patch and removed after the run: `test-late-panic` on `virt` at EL2, waited to
QEMU's stop. `cargo test --test toyos-build -- virt_scout` exited 0:

```
[kernel 1.557 cpu0] panic: rebooting in 60 s, timed by the calibrated clock
[scout] QEMU's stop reason Some("guest-reset"), 60.0s after the ready marker was read
[scout] said after the ready marker: Ok("\npanic: the bound is over: returning this machine to firmware\n")
[scout] CPU_OFF, SYSTEM_OFF and SYSTEM_RESET calls traced: [(84000009, 0)]
```

The patch is `s0-scout-panic-resets.patch` in
https://github.com/ToyOSOrg/ToyOS/pull/647#issuecomment-5956924013.

**Exit**: that patch's test as a `virt_` row, red when `reboot_now` halts
instead of resetting, once one boot can shorten the bound, or on the owner's
word that the row is worth its 60 s.

Owner: `kernel/src/panic_reboot.rs`, under
`issues/kernel/toyos-runs-on-arm64.md`.
