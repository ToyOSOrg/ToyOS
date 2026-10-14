---
status: open
kind: defect
opened: 2026-09-03
---

# The kernel cannot tell "no display" from "a display that published no framebuffer"

Both arrive in `KernelArgs` as zeros, so `kernel/src/main.rs:284-288` records the
same line for a machine with no display device at all and for one whose firmware
published a Graphics Output Protocol that has no linear framebuffer to hand over.
Three shapes, one image, the guest's own 16550:

```
== no-display            (-vga none)
[kernel 0.000 cpu0 boot] boot: gop 0x0+0x0 0x0 stride 0 format 0

== blt-only-display      (-vga none -device virtio-gpu-pci)
GOP: 1280x800 is Blt-only, so this display publishes no framebuffer
[kernel 0.000 cpu0 boot] boot: gop 0x0+0x0 0x0 stride 0 format 0

== framebuffer           (-vga std)
GOP: 1280x800 stride=1280 format=1 fb=0xc0000000 size=4096000
[kernel 0.000 cpu0 boot] boot: gop 0xc0000000+0x3e8000 1280x800 stride 1280 format 1
```

The first two kernel lines are identical. The only thing that separates them is
the bootloader's line above the second — printed by `query_gop`
(`loader/src/main.rs`) on whatever console firmware left behind, which a
metal-sim boot with `mute: true` and the T14 itself do not have. Nothing reaches
the kernel, so nothing reaches `/log`, the panel, or a crash report.

`kernel/src/main.rs` then branches on `kernel_args.gop_framebuffer != 0` and
takes the same arm for both: both print `GPU: the firmware handed over no
framebuffer`, measured for the Blt-only shape on `Profile::HeadlessVirtioGpu`
(`the_compositor_drives_the_virtio_gpu`'s boot log). There the compositor's
`pci:1af4:1050` claim draws the desktop, but nothing in the kernel's record
tells the two shapes apart, and on a machine whose Blt-only display no
process drives — the T14, until an Intel display engine driver exists — the
record is all there is.

The distinction is the difference between "this machine has no panel" and "this
machine has a panel and something upstream refused it", which are different
things to go and look at.

**What would close it.** A reason, carried in `KernelArgs` beside the geometry
and printed on the `boot: gop` line, so a zero framebuffer says which zero it is.

Found while landing the mode-inheritance half of the 2026-09-01 GOP ruling, which
is what made a refusal reachable at all: before it, `query_gop` returned the same
`None` for every unusable format and said nothing either.
