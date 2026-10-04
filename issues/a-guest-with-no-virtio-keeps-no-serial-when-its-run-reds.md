---
status: open
kind: tooling
opened: 2026-09-27
---

# A guest with no virtio device keeps no serial when its run reds

`tests/common/lane.rs`'s `keep_serial` copies a red run's `uart-*.log` files to
`target/red-run-serial`. A profile with no virtio device (`qemu_command`'s
`shape.virtio.present()` false: `Profile::Metal`, and every `power.rs` guest
built on `panicked()`) routes its 16550 to QEMU's stdio instead, so its only
record is the reader thread's memory, and the kept lane directory is empty. The
console stream of a virtio guest is not kept either; only its UART is.

**Evidence:** `syscall_fault_halts` red at `8e525da4`: the run printed
`this red run's serial logs are kept at target/red-run-serial/toyos-tmp-45425-0`,
and that directory's `lane-0` is empty. The check's own error carried no
capture, so what the kernel said was lost.

**Exit condition:** every guest's console, whichever device carries it, is
written under its lane as it is read, and a red run keeps it; a red
`panic_reboots` run shows a non-empty kept lane.
