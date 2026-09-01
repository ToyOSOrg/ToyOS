---
status: open
kind: defect
opened: 2026-07-31
---

# The UEFI GOP path is off by default, and picks an absurd mode when on

Until `06ce633` no configuration in this tree produced a UEFI GOP at all:
`kernel/src/drivers/gop.rs` had never executed and `kernel_args.gop_framebuffer`
was zero everywhere. `cargo run --gop` and `BootOptions { profile: Profile::Gop }`
(`-vga std`) fixed that and the path works, but two residuals remain.

**The owner ruled on 2026-09-01: GOP is the floor, and the mode is the
firmware's.** Two things follow, and neither is a preference.

The mode policy is not a choice between candidates: no operating system picks
the largest mode. The firmware has already negotiated with the panel over EDID
and set the display's own mode, and an OS inherits it — Linux's `efifb`, now
`simpledrm`, exists to do exactly that and never does modesetting itself. So
`bootloader/src/main.rs`'s "most pixels wins" is replaced by the mode already
set, and the 2048x2048 square goes with it.

GOP is also not a profile to enable. It is the stage every physical machine
boots through, and virtio-gpu is the accelerated stage that only exists inside
a VM — on the T14 there is no virtio-gpu at all, and until an Intel display
engine driver exists the compositor sits on the GOP framebuffer directly. So
GOP becomes the path every boot takes, with virtio-gpu layered above it where
the machine has one.

The 118 ms against 188 ms below prices the bug and not the feature: it was
measured at 2048x2048, and a repaint costs per pixel, so a 1920x1080 panel is
roughly half the pixels. Re-measure after the mode fix. Do not carry the 70 ms
into the decision it was taken to inform.

**It is not the default.** Plain `cargo run` and the default test config still
boot `-vga none` with virtio-gpu or with no display device, so `gop.rs` is
exercised only by `--gop`, by `--metal-sim` (which every machine test now
boots), and by the screen tests that boot a guest at all. Every other test in
the suite still says nothing about the display path a laptop takes.

**The mode is wrong.** `bootloader/src/main.rs:186-205` selects the mode with
the most pixels. On QEMU stdvga that is **2048x2048** — square, non-standard,
and it makes the compositor scale a 1920x1080 wallpaper to a square. It is also
16 MiB of framebuffer, which is what makes a panic-console repaint cost ~13 ms.
"Largest wins" is not a mode policy; a real one would prefer the firmware's
current mode, or the largest 16:9/16:10 mode, and only then fall back. Harmless
for a first boot, wrong once the compositor owns the screen — and that shipped
without fixing it, so the compositor scales a 1920x1080 wallpaper onto a
2048x2048 square on every metal-sim boot
and each panic screendump is 12 MiB. On the T14 the firmware will offer the
panel's own mode and "largest wins" may or may not pick it; that is the part
nothing here can answer.

**What the repaints actually cost.** `e5e600f`'s message gives two figures that
cannot both be true — "~13ms per repaint" and "135ms to 181ms" for six of them.
Measured A/B in one session on this host: the same tree boots to `Boot:
complete` in **118 ms** with no console armed (`-vga none`) and **188 ms** under
GOP. Five repaints happen before that line is logged and the sixth after it, so
the per-repaint figure is right (~14 ms) and the 135→181 pair is the wrong one.
Every phase boundary also carries a `wbinvd`, which QEMU ignores entirely — on
metal that is six full cache-hierarchy flushes on a machine that keeps running,
and it is not measurable here.
