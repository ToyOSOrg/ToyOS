---
status: open
kind: defect
opened: 2026-08-03
---

# A machine with no console says nothing between `Boot: complete` and the terminal

**Measured under metal-sim, and worse than "no scrollback".** With
`--metal-sim --mute` and no virtio-console the guest has no output channel at
all once the last boot checkpoint has painted: the failure screen ends at
`Boot: complete`, and soundd's null-sink line and netd's exit line — printed seconds later,
and read directly off the console by `metal_sim_compositor` on the same machine
shape with the 16550 on — reach no pixel and no file. A running ToyOS on the
T14 is mute between `Boot: complete` and the moment the compositor's terminal
exists. That is fine for a first boot and not fine for debugging input on the
machine. It is also the entire cost the mute default was buying, which is why
the metal-sim profile now keeps its 16550 by default.

Narrowed but not closed: the last checkpoint now paints where this boot's log
can be read (`main.rs`'s `report_log_destination`), so the panel says whether
there will be anything to go back to. What it cannot give that machine is a
line *after* the checkpoint.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling). It is measured
rather than suspected, and what it costs is the window in which input debugging
on real hardware happens: a T14 running ToyOS says nothing between
`Boot: complete` and the moment the compositor's terminal exists. Owed by
whoever gives that window an output channel; `report_log_destination` narrowed
it by saying where the log will be, and cannot put a line in it.

## The shipping image's half of the same window, folded in

`panic_console::boot_checkpoint` returns immediately once
`SCREEN_OWNED_BY_USERLAND` is set
(`kernel/src/drivers/panic_console/mod.rs:639`), and a compositor claiming the
framebuffer sets it. So on `bootable.img` the last kernel screenful ever painted
is the one at `Boot: complete`, the desktop overwrites it a few tens of
milliseconds later, and no key pauses it: `page_forever` is reached only from
`halt_all_cpus`, so a *successful* boot never pages.

**The durable answer landed and is not this.** "A log sink that survives
userland" is `/system/bin/logd`: the kernel keeps the record ring and the console and
writes no file at all, logd owns `/log` and puts one file per boot there named
for the wall clock, `src/build.rs`'s `every_boot_config_runs_logd` refuses a boot
config that omits it, and `kernel/src/log_file.rs` is deleted. Both ways of
reading that file need what this window denies: pulling the stick takes the
machine out of the session, and `cat /log/<newest>` from the desktop needs input,
which is the case this was opened for — a dead keyboard and a dead TrackPoint,
nothing to launch or type with. The residual is narrower than "no scrollback"
and not empty: **live readability, on the machine, with no working input and no
reflash.**

**Two diagnostic modes exist and neither reaches the shipping image.**
`--diag-boot` builds `target/bootable-diag.img` with no compositor in the init
list, so `boot_checkpoint` keeps painting; `screen_diag_boot` gates it, and its
`[page n/m]` footer branch has never executed — QEMU's grid fits the log on one
page where the T14's 240 columns give two, so that branch is a guard and not a
certification until the laptop runs it. `--console-boot` puts `/system/bin/console`
over the framebuffer with a shell underneath, but claiming the screen is exactly
what stops `boot_checkpoint` painting, so a machine that wedges *before* userland
shows nothing at all in that mode; its scrollback seed is a file read once at
startup rather than a cursor, and it needs a `/log` that only a USB boot mounts.

**What the panel cannot show even when it does paint.** `glyph`
(`kernel/src/drivers/panic_console/mod.rs:1076`) maps every byte outside
`0x20..=0x7E` to `.`, so a three-byte UTF-8 `—` is three dots in three columns
instead of one. Measured on this tree: `rg -o 'log!\(' kernel/src | wc -l` is
686 sites and `rg -n 'log!\(.*—' kernel/src | wc -l` is 57 of them. Cosmetic
under QEMU; not cosmetic against the T14's 240-column wrap, which decides whether
a line is one display row or two and therefore whether it is on the page the
checkpoint paints. The cheap fix is to render the sequence as a single `-`; the
honest one is to keep non-ASCII out of `log!`.
