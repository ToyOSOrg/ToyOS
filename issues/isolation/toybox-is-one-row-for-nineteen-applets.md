---
status: open
kind: defect
opened: 2026-08-20
---

# `[programs.toybox]` is one authority row for every applet in the binary

Noticed while giving `/system/bin/shutdown` the `power` right
(`toyos-abi/src/handle.rs`'s `Rights::POWER`).

A manifest row's granularity is the **binary**, and `/system/bin/toybox` is nineteen
programs behind nineteen symlinks. `/system/bin/init`'s `declared` resolves
`/system/bin/shutdown` through `std::fs::read_link` to `/system/bin/toybox` and endows the
`toybox` row (`userland/init/src/main.rs`), so every applet in the image is
spawned holding the union of what any of them needs.

This is not new — `system.toml`'s `toybox` row already hands `ls` a connector to
the compositor and one to soundd, because `screen` and `tone` need them. What is
new is the size of the largest member of that union: `power` ends the machine,
so `/system/bin/echo` in the shipped image now holds a capability that cuts the power,
where before it held two connectors it does not use.

## What it is not

It is not the closed isolation defect that `SYS_SHUTDOWN` demanded no
capability at all. Before that change
*every process in the machine* could halt it with an argument-less syscall;
now the set is one program's row plus `/system/bin/init`. The remaining exposure is a
program a person deliberately runs from a shell, not a daemon endowed one
connector.

## What would fix it

A `[programs]` key whose row is chosen by the **invoked path** rather than by
the binary the symlink resolves to. `declared` already tries the caller's own
path first (`row(path)` before `read_link`), so a row keyed `shutdown` with
`path = "/system/bin/shutdown"` would win — but nothing builds such a row today: a
`[programs]` key names a crate the build compiles, and `bin/shutdown` is a
`[symlinks]` entry with no crate behind it. Making the build able to declare an
authority row over an existing binary is the work, and it is worth doing only
if the union keeps growing.

## Its size is measured now, and it is a number

`endowment_denied`'s `every_applet_holds_only_what_its_policy_names` reads the
links off `/system/bin` and the rows off `/system/etc/system.manifest`, neither through
`declared`, and holds each applet against a per-applet policy table. On the
image a guest test boots it answers `14 links behind /system/bin/toybox, 13 declared
over-grants` — thirteen applets endowed a connector to soundd because `tone`
needs one. `DECLARED_OVER_GRANTS` is that list, so the over-grant cannot grow
without a red naming what grew, and it shrinks when the row is split.

The shipped image is not that image and is still unmeasured: a guest boots
`tests/testcases/system.toml`, whose `toybox` row carries no `syscap` at all, so
nothing yet sees `/system/bin/echo` holding `Rights::POWER`.
`issues/isolation/one-manifest-row-grants-nineteen-applets-and-one-is-compared.md`
carries what closing that costs.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling). `/system/bin/echo` in
the shipped image holds `Rights::POWER` because `/system/bin/shutdown` is behind the
same binary — an authority over-grant that is true of the image as built, not a
hypothetical. Owed by whoever makes the build able to declare a `[programs]`
row over an existing binary keyed by the invoked path; `declared` already tries
`row(path)` before `read_link`, so the resolution half exists and the build half
does not.
