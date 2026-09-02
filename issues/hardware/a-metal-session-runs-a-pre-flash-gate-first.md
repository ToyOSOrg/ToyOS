---
status: open
kind: track
opened: 2026-08-01
---

# A metal session runs a pre-flash gate first, and the gate is a written verdict

The T14 has no serial: the 16550 loopback reads `0xFF`, so the on-screen console
is the entire diagnostic channel and anything it cannot show is silent. Flashing
a bad image costs a session. This is what a session does before it flashes, and
it is a *written* verdict — pass, fail, or read-verified-only with the reason —
frozen with its date, not a checklist someone ticks.

**Run it on a quiet tree. No-go on any unresolved false pass, even where the
command printed success.**

Audit the delta first: every commit to the kernel, bootloader, build system, ABI
and SDK since the last such verdict, by hash, with a "boot is unchanged" item for
anything touching the boot path.

1. **Nothing may write to a disk it was not given.** The highest-consequence
   section and the one the harness cannot catch. Read the designation stamp's
   definition and every hunk of history touching it and the filesystem adapter.
   Establish that format and mount are not public and that every raw block write
   is downstream of the probe — by *enumerating callers*, not by matching three
   names, and remembering that an enumeration which skipped the cargo git
   checkouts is not an enumeration. *False pass:* a new caller reaching a write
   path without consulting the stamp, or a check that warns and continues.
2. **The image is flashable.** Measure the built file's size **on disk** and
   confirm it is a whole number of 512-byte sectors — the build's own assert
   covers the computed size, not the file. Confirm `EFI PART` in the *final*
   sector: a healthy primary GPT hides a missing backup.
3. **Boot-time panics stay closed** — for each, the guard exists *and* a test
   exercises the absent-device path. A diskless boot. The required-CR4
   declaration. The framebuffer extent computed from stride, not width — on QEMU
   they are equal, so read the expression. An xHCI boot with zero HID devices,
   confirmed from the log line rather than from the return type. Expect two of
   these to be read-verified only: TCG reports every CPU feature present, so the
   missing-bit path cannot run.
4. **The on-screen console. If this fails, do not flash.** Every screen test.
   Confirm the muted profile actually removes the UART, and that the paging test
   is driven by a timer rather than a keypress — input may be dead on the
   machine. *False pass:* the late-panic gate passes with the capture routine's
   body replaced by a bare return, so these cover rendering, not capture.

**State to the owner before he boots**, because it is the difference between a
diagnosis and an afternoon: a refusal to attach to the keyboard is the driver
working, not a regression; the touchpad is I2C-HID and unbuilt, so a dead
touchpad is the expected outcome and must not consume debugging time; and TCG
cannot measure the 2× bar, so performance is what the session is for.

The session checklist itself — the measurements only silicon can close — is
carried here too:

| measurement | closes |
|---|---|
| one boot with AP control-register inheritance armed against one without, same image, same session; record the delta | `issues/kernel/ap-control-registers-inherit-init.md` |
| transcribe the 16550 loopback line, and the boot's own completion time beside the last metal-sim reading of the same image | the T14 half of the console-drain question |
| re-take the phase breakdown below on the boot the session flashes | whether the peripherals phase is still 73% of a metal boot |

**That second row closes half an obligation, not the whole of it.** What is owed
is what an inline drain costs when every boot record goes synchronously to a
115200-baud port. QEMU answers instantly and cannot price it, and **the T14
cannot price it either** — it has no SuperIO, so the loopback probe reads `0xFF`,
`has_console()` is false, and the mode is a branch not taken. What a metal
session *can* close is exactly that: the probe byte is the evidence that the T14
pays nothing, which is what `kernel/src/log/console.rs` claims about this machine
and what a flashed image depends on. The 115200-baud arm needs a machine with a
real port and stays open until there is one; the arithmetic for it — ~40 KB at
~87 µs/byte, so seconds — is a prediction and says so.

`issues/hardware/pre-flash-gate-missed-the-milestone.md` records that the last
verdict certified everything except input.

## What a metal boot costs, so a session does not re-derive it

Taken from the 2026-08-07 freeze capture, not from a datasheet: six healthy
boots report `Boot: complete` at 1148, 1148, 1149, 1150, 1151 and 1154 ms, and
the seventh at 755 ms is the control boot whose keyboard was refused, so its
peripherals phase is 448 ms instead of 842. The comparable QEMU shape is
`Boot: complete (196ms)` on the `metal_sim_compositor` boot
(`cargo test --test toyos-build -- metal_sim --nocapture`) and `(234ms)` for the
diag artifact booted headless. **Metal is therefore ~5.9× QEMU, not the ~17× a
superseded inventory computed** — that ratio was against a 3422 ms boot of which
2.30 s were six `boot_checkpoint` framebuffer repaints, since removed; the
phase-boundary gaps for all six in the longest of the seven boots total 73 ms.
Any metal boot timing carried forward from that inventory describes a machine
that no longer exists.

| phase | reported |
|---|---|
| CPU ready | 60 ms |
| storage ready | 84 ms |
| **peripherals ready** | **842 ms** |
| subsystems ready | 93 ms |
| devices ready | 20 ms |

Peripherals is 73% of the boot. Its two largest components are 393 ms of i8042
keyboard init (`i8042: ok selftest=0x55` at 0.609, the next i8042 line at 1.002 —
real hardware, not a probe of an absent one) and 206 ms establishing that the
Thunderbolt xHC at `00:0d.0` has nothing on any port (`controller started` at
0.161, `no HID devices on the controller at 00:0d.0` at 0.367; four of the PCH's
port resets are 55 ms each, which is USB's own). **Absent-device probing is
~279 ms of the 1151, not ~1.1 s**: the rest is the PCI walk, 73 ms scanning buses
that hold nothing against 7 ms finding everything that is there
(`PCI: Enumerating devices...` 0.065, last real function `0a:00.0` 0.072,
`Enumeration complete, 24 functions.` 0.145).

The record this came from also warned about a NIC retry that looks like boot
cost; that warning is spent. `toyos/src/net.rs:272` says the hundred retries at
ten milliseconds are gone — a `netd` connector is live from a process's first
instruction — so nothing after `Boot: complete` inflates a reading any more.

The panic path's own metal price belongs to the same session: the T14 measured
461/459 ms repaints. The open question is painter granularity — a glyph
assembled in a scratch row and blitted as one run would merge where the per-bit
`write_volatile` stores do not — and what rules out the obvious fix is the
invariant at `kernel/src/drivers/panic_console/mod.rs:542`: render and everything
it calls takes no lock, so a shared static scratch strip re-creates the
multi-CPU race the panic-path records carry against `capture()`.
