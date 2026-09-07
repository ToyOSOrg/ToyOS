---
status: open
kind: tooling
opened: 2026-09-07
---

# No boot that panics can be judged on the T14

Three of this project's kernel-performed resets are reachable under QEMU and
judged there by `usb_reset_hands_devices_back`: a job list's `reboot`, the test
runner's job deadline, and the panic console's bound. Only the first two reach
the T14, and the reason is the loop rather than the kernel.

**Two walls, and a boot has to clear both.**

1. `src/metal.rs`'s `drive` ends every round with
   `bootlog::verdict(&log).map_err(Refusal::Log)?`, which requires
   `Boot: complete (Nms)` *and* a trailing `Rebooting.`. A boot whose subject is
   a panic correctly writes neither the second word nor anything after it, so
   the loop refuses it — the readback is still written, and
   `tests/common/metal.rs`'s `Mode::Drive` tolerates the non-zero exit, but the
   boot-level rows `tests/metal-profile.toml` prices are then judged against a
   log the loop has already called unfit.

2. `test-late-panic` fires in `kernel_main` after `spawn_init` and before
   `enter_idle_loop`, so `logd` has not run: that boot writes no `/log` file at
   all and `boot.<label>.complete_ms` has nothing to read. `metal-panic-probe`
   fires late enough — five seconds after a process claims the framebuffer — but
   `src/metalimage.rs`'s `derive` appends `reboot` to every arm's job list
   unconditionally, so the runner hands the machine back at about one second and
   the probe never comes due.

## What it costs

The panic path's register stop (`kernel/src/drivers/xhci/stop.rs`) is the one
arm of the ruling "no reset this kernel performs leaves a USB device
mid-command" that only QEMU has answered. QEMU cannot wedge a stick, so what is
unproven on hardware is exactly the case the ruling is about: the machine's own
controllers, its own stick, and a reset with no shutdown in front of it.

## What would clear it

A per-boot predicate in place of `bootlog::verdict` as the loop's judge — the
audit at `t14-suite-list.md` finding 6 asks for the same thing for
`log_partition_identity` and `root_named_but_absent` — and either an arm that
may decline the derived `reboot` job, or a panic actuator that fires after
`logd` is up and before the runner's first job returns.
