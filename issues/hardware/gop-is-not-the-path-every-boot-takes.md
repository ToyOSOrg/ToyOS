---
status: open
kind: defect
opened: 2026-09-03
---

# GOP is not the path every boot takes

Until `06ce633` no configuration in this tree produced a UEFI GOP at all:
`kernel/src/drivers/gop.rs` had never executed and `kernel_args.gop_framebuffer`
was zero everywhere. `cargo run --gop` and `BootOptions { profile: Profile::Gop }`
(`-vga std`) fixed that and the path works.

**The owner ruled on 2026-09-01: GOP is the floor, and the mode is the
firmware's.** The mode half has landed. This file is the other half, and it is
not a preference.

GOP is not a profile to enable. It is the stage every physical machine boots
through, and virtio-gpu is the accelerated stage that only exists inside a VM —
on the T14 there is no virtio-gpu at all, and until an Intel display engine
driver exists the compositor sits on the GOP framebuffer directly. So GOP
becomes the path every boot takes, with virtio-gpu layered above it where the
machine has one.

**It is not the default.** Plain `cargo run` and the default test config still
boot `-vga none` with virtio-gpu or with no display device, so `gop.rs` is
exercised only by `--gop`, by `--metal-sim` (which every machine test now
boots), by the screen tests, and by `screen_gop_firmware_mode`. Every other test
in the suite still says nothing about the display path a laptop takes.

**Why it is its own landing.** `Profile::Headless` is what the suite's
non-display profiles are, so giving the floor a display changes the boot config
of every test at once — `tests/common/qemu.rs`'s profile table, `src/qemu.rs`'s
own, and every assertion that reads a console on a machine that then has a
panel too.

**The mode half, closed.** `bootloader/src/main.rs`'s `query_gop` reads
`Mode->Info` and never calls `SetMode`, so the kernel is handed the mode the
firmware already set. A profile says what panel its display advertises over EDID
(`Shape::panel`) and the firmware sets that mode: `Profile::Metal` advertises
the T14's 1920x1080 and metal-sim's screen is now the laptop's, 240x67 cells
with the 8-pixel strip below them. `screen_gop_firmware_mode` is the gate —
QEMU's scanout geometry over QMP against the kernel's own `GOP:` line, on two
machines advertising different panels.

**What the repaints cost, re-measured.** `e5e600f`'s message gave two figures
that cannot both be true — "~13ms per repaint" and "135ms to 181ms" for six of
them — and the 118 ms / 188 ms pair that replaced them priced the bug, at
2048x2048. Ten boots of each arm in one session, alternating, two images
differing only in `query_gop`, median of the guest's own `Boot: complete (Nms)`:

| arm | median | samples |
|---|---|---|
| `-vga none`, base image | 274 ms | 269..276 |
| `-vga none`, fixed image | 273 ms | 269..276 |
| GOP, 2048x2048 (base) | 381 ms | 376..385 |
| GOP, 1280x800 (fixed) | 307 ms | 302..310 |

Five of the six `boot_phase!` repaints land inside that window
(`kernel/src/main.rs:352,395,415,471,532,551`; the sixth paints after its own
timestamp), so one repaint costs 21 ms at 2048x2048 and 7 ms at 1280x800 — a
4.10x pixel ratio bought 3.1x, the rest being per-paint work that does not scale
with the panel. The `-vga none` pair is the control: off the GOP path the two
images are one millisecond apart.

The sentence this replaces said each phase boundary also carries a `wbinvd` QEMU
ignores, so metal would pay six cache-hierarchy flushes the measurement could not
see. It stopped being true at `1134414e`, which made the scanout write-combining
and left `flush_stores` — one `SFENCE`, `kernel/src/drivers/panic_console/mod.rs:944` —
in its place; `grep -rn 'wbinvd' kernel/src/drivers/panic_console/` finds nothing.
No unmeasured metal cost hides behind these numbers on that account.
