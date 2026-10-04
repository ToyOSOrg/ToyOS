---
status: open
kind: tooling
opened: 2026-09-28
---

# The kernel console split does not re-arm across a guest reset

`src/kernelconsole.rs`'s `KernelConsole` withholds the virtio port's bytes
until the kernel's first record (`toyos_logstream::kernel_opening`) and
passes every byte after it: `held` is
`None` from then on. A guest reset inside one QEMU process does not re-arm it.
`tests/common/update.rs`'s `Rig::boot` boots the Headless profile, whose
stdio is that port, with `takes_the_reset`, and each `reboot` resets that
machine in place. Whatever the next boot's firmware and loader write on the
port reaches every stdio reader as kernel console.

What the next boot writes there has never been captured. The first boot's
stream is measured. On QEMU 11.1.1's own edk2 it opens with
`ESC[2J ESC[01;01H ESC[=3h ESC[2J ESC[01;01H` twice, then
`BdsDxe: loading Boot0001 …`, as a failing test's console dump in the
orchestrator's #572 round-2 nightly shows. No log from #572's runs shows the
port after an in-process reset: each carries that prelude only where a stream
starts. So no signal is known to be in the stream at a reset. Re-arming on the
first boot's prelude would rest on the guess that the firmware repeats it.

Nothing reds on it. The update tests look for a needle past an offset in the
console (`owed`, `reboot_until`), and firmware lines beside the needle do not
stop it matching. Every update test passed in #572's round-2 and round-3
nightlies. If the firmware does write its handoff line there,
`Loader log: … so [ 1.234 cpu0 kernel] …` is a line `Serial::interleaved`
reports as a torn
kernel line.

## Exit condition

A capture of stdio across an in-process reset on a virtio profile. If the
firmware writes on the port there, the split re-arms on a signal that capture
shows, with a host test over the captured bytes. If it writes nothing, this
file is deleted with the capture in the commit.

## Owner

`src/kernelconsole.rs` and `tests/common/qemu.rs`'s stdio reader; unheld.
