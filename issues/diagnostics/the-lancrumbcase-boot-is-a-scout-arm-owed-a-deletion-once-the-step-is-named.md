---
status: open
kind: tooling
opened: 2026-09-21
---

# `tests/lancrumbcase` is a scout arm, and it is owed a deletion once the step that ends the T14 is named

`tests/lancrumbcase` is `tests/lanphycase` with a different `args` row — netd's
`--exit-with-crumbs`, under which netd brings the I219 up exactly as
`--exit-with-phy-outcome` does and ends with the same code, and before every
call on the claim and every register access appends one line to
`/log/crumbs.txt` and `fsync`s it to the stick. It exists because boots of the
T14 that hand the I219 to netd leave the machine powered off about half the
time, the same image passing and dying, and every one of those boots leaves the
same record: `logd`'s first three rounds and nothing after. A power-off seals
no black box and outruns `logd`, so nothing on the stick says which step the
machine ended in. The last line of the crumb file does.

**It cannot see the kernel's hand-over.** `/system/bin/init` claims the
function before it spawns netd, so `pcidev::claim` — the BAR placement, the MSI,
the IOMMU domain, bus mastering — has returned before netd's first instruction.
A dead boot with no crumb file at all is a machine that ended before netd's
first line was durable, and the hand-over is inside that; separating it takes a
crumb in init, which this arm does not have.

**It slows the window it measures.** Each crumb is a stick write. In front of
QEMU's 82574 `lan_crumb_trail` measured 49 crumbs at 1,960 µs each, the slowest
14,415 µs, and netd holding the card for 140 ms where the same boot without the
trail holds it for 40 ms. The T14's stick has not been measured, and every
crumb line carries the clocks that will.

It is `lan_crumb_trail`'s metal row, a fifth image and six rows of
`tests/metal-profile.toml`.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

The step named — by a run of dead boots whose trails end at one crumb, or by
trails that end at different ones, which says the machine's end is not any
step of netd's — and the power-off explained or handed to whoever owns what it
points at. Then the arm goes, all of it: `toyos-i219/src/crumbs.rs` and its
tests, `userland/netd/src/crumbs.rs`, the `trail` parameter of
`userland/netd/src/i219.rs`'s `registers` and `granted` with `leave_crumbs`,
netd's `--exit-with-crumbs` and the second parameter of its `CARDS`
constructors, `tests/lancrumbcase`, `tests/e1000crumbcase`, their two rows in
`src/build.rs`'s `ALL_CONFIGS` and the fourth entry of its `INTEL_ACTUATORS`,
`lan_crumb_trail`'s metal row in `tests/toyos.rs` with `LANCRUMBCASE`,
`lan::trailed_on_metal` and `Readback::log_volume_file`, the
`lan_crumb_trail` registration, and the six
`tests/metal-profile.toml` rows.
