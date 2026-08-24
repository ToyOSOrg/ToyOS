---
status: open
kind: defect
opened: 2026-08-08
---

# Four driver waits spin with no deadline, and NVMe's `init` reads the timeout the spec gives it and throws it away

Re-measured against the tree on 2026-08-24. Still four, still all on the boot
path, and the line numbers below were re-read at `739af0c2`:

- `kernel/src/drivers/nvme.rs:945-947` — `while bar.read_u32(REG_CSTS) & 1 != 0`
  (controller disable), in `init`.
- `kernel/src/drivers/nvme.rs:973-975` — `while bar.read_u32(REG_CSTS) & 1 == 0`
  (controller enable), in `init`.
- `kernel/src/drivers/virtio.rs:640-645` — `Virtqueue::submit_and_wait` polls
  `poll_used()` forever. (Its *panic-path* instance is filed in
  `issues/panic-path/`; this is the ordinary one.)
- `kernel/src/drivers/virtio.rs:779-781` — device reset,
  `while common.read_u32(COMMON_DEVICE_STATUS) != 0`, in `VirtioDevice::init`.

The fifth was `nvme.rs`'s `wait_completion`, which every admin and I/O command
reached through `submit_and_wait` and which spun on the completion-queue phase
bit with no deadline. **Closed 2026-08-20**: it is bounded by `nvme.rs`'s
`COMMAND` budget through `clock::settles`, and the composition above it by
`block::OPERATION` between commands. That says nothing about the two `CSTS.RDY`
polls below, which are a different wait with a different number.

**NVMe hands the driver the bound and `init` drops it — `reset` does not.**
`CAP.TO` (bits 31:24, in 500 ms units) is defined as the worst-case time for
exactly the `CSTS.RDY` transitions above and for nothing else. `nvme.rs:939`
reads the whole `CAP` register and `:940` takes `((cap >> 32) & 0xF)` — the
doorbell stride — out of it; nothing else in `init` touches `cap`. So on that
path the one number the device publishes about how long to wait is read into a
local and discarded, and a controller that never sets `RDY` hangs the boot with
nothing on the log to say which one.

**`NvmeController::reset` is the worked example, and it is in the same file.**
`nvme.rs:511-536` takes `((self.bar.read_u64(REG_CAP) >> 24) & 0xFF).max(1)`,
turns it into a `time::Bound::from_register` naming "NVMe CAP.TO, the
controller's own worst case for a `CSTS.RDY` transition", and gives both of its
own `RDY` waits to `clock::settles` against it — refusing with
`NVMe: reset failed: CSTS.RDY would not clear in {ready}` rather than spinning.
`init`'s two polls are the same two transitions on the same register with the
same published bound, six hundred lines above, unbounded.

**The primitive is shared now, which is what makes the four remaining sites a
choice rather than a cost.** `crate::clock::settles` (`kernel/src/clock.rs:153`)
is the one bounded device-register wait in the kernel: it takes a nanosecond
budget, polls against a TSC deadline, and returns `false` for the caller to turn
into a refusal that names the register. `nvme.rs`'s `COMMAND` and `reset` paths,
`hda.rs`'s four reset waits and `xhci/wait/mod.rs`'s `settles` all call it, and
`xhci/legacy.rs`'s handoff is bounded by a `Budget` deadline of its own
(`:60`, `:186-189`). The duplicate copies this entry recorded — a second body in
`hda.rs`, a third in the deleted `hda_probe.rs`, an IOMMU variant that
`assert!`ed where the others returned — are all gone. `settles`' own doc records
why consolidating the *other* way is refused: reading `nanos_since_boot` per
iteration is a `u128` divide that an instruction-pointer sample attributes to
`compiler_builtins` instead of to the loop.

**Standing.** The kernel-drivers type-safety audit's F10 (deadlines and
durations as bare `u64` in two different units, so `wait_writable(500)` compiles
and means "expired at boot") and F11 (the
`wait(off, until, pred) -> Result<u32, Timeout>` primitive and its blast radius)
are the design; F11's own closing line is "**Standing.** Not filed." Two
corrections to it: its count of eight unbounded MMIO polls is **four** today,
because the xHCI sites it named closed and the NVMe completion wait closed after
them; and `CAP.TO` appears nowhere in it. **Not** the completion track — that
owns the *park* deadline (`Instant`/`Duration`/`Deadline`, "no `0 = forever`"),
never a driver register poll, and it does not touch NVMe at all.
