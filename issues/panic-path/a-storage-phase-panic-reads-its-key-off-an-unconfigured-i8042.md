---
status: open
kind: defect
opened: 2026-09-27
---

# A panic in the storage phase reads its key off an i8042 the kernel has not configured

The panic panel's reset bound is retired by a key press
(`kernel/src/drivers/panic_console/mod.rs`, `read_key`), which polls port
`0x60` through `keyboard_controller::poll_byte`. `i8042::init`
(`kernel/src/arch/x86_64/i8042/mod.rs`) is what turns translation on, stops
and restarts scanning and enables the port-1 clock. It now runs first in the
device phase (`arch::boot::platform_devices` in `kernel/src/main.rs`), after
storage, so its lines stay on a panel that shows the log's tail.

So a panic anywhere in the storage phase — NVMe or xHCI init,
`rootfs::hold_source`, the DATA and FAT mounts — meets the controller as the
firmware left it. Whether a key press then reaches `read_key` as a set-1 make
code depends on the firmware: with translation off or scanning off it does
not, and the panel resets at its bound however many keys are pressed. That
ordering is the one the tree had before storage moved behind init's spawn, so
it has shipped before; it is still a weakness. No boot exercises a panic in
the storage phase with the firmware's controller state left unconfigured.

## Exit condition

A key press retires the panel's bound for a panic anywhere after the i8042
probe could have run — the controller configured before the first phase that
can panic on a device, without moving its lines off the panel's tail — and a
boot that panics in the storage phase on the metal-sim shape shows the press
retiring it.
