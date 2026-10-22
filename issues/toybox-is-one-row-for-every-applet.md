---
status: open
kind: defect
opened: 2026-08-20
---

# `[programs.toybox]` is one authority row for every applet in the binary

A manifest row's granularity is the **binary**, and `/system/bin/toybox` is many
programs behind that many symlinks. `/system/bin/supervisor`'s `declared`
resolves `/system/bin/shutdown` through `std::fs::read_link` to
`/system/bin/toybox` and endows the `toybox` row
(`system/supervisor/src/main.rs`), so every applet in the image is spawned
holding the union of what any of them needs.

In the shipped image (`images/system.toml`) that row receives the
`compositor`, `soundserver`, `surface`, `power` and `display` connectors and
holds `syscap = ["roster"]`. `power` is the connector `shutdown` and `reboot`
ask the supervisor to stop the machine through (`system/toybox/src/shutdown.rs`,
`reboot.rs`), so `/system/bin/echo` in the shipped image can end the machine,
and `roster` is `ps`'s.

## What it is not

It is not the closed isolation defect that `SYS_SHUTDOWN` demanded no
capability at all. Before that change *every process in the machine* could
halt it with an argument-less syscall; now the set is the rows that receive
`power`. The remaining exposure is a program a person deliberately runs from a
shell, not a daemon endowed one connector.

## What would fix it

A `[programs]` key whose row is chosen by the **invoked path** rather than by
the binary the symlink resolves to. `declared` already tries the caller's own
path first (`row(path)` before `read_link`), so a row keyed `shutdown` with
`path = "/system/bin/shutdown"` would win — but nothing builds such a row today:
a `[programs]` key names a crate the build compiles, and `bin/shutdown` is a
`[symlinks]` entry with no crate behind it. Making the build able to declare an
authority row over an existing binary is the work.

## Its size is measured on the test image only

`endowment_denied`'s `every_applet_holds_only_what_its_policy_names`
(`tests/guest/rust-tests/src/bin/endowment_denied.rs`) reads the links off
`/system/bin` and the rows off `/system/etc/system.manifest`, neither through
`declared`, and holds each applet against `APPLET_NEEDS`. On the image a guest
test boots, `tests/images/testcases.toml`, whose `toybox` row receives
`soundserver` alone, every applet but `tone` holds that connector without
needing it; `DECLARED_OVER_GRANTS` is that list, so the over-grant cannot grow
there without a red naming what grew.

The shipped image is not that image and is still unmeasured: nothing yet sees
`/system/bin/echo` holding `power`.
`issues/one-manifest-row-grants-every-applet-and-one-is-compared.md` carries
what closing that costs.

**Exit**: each applet behind `/system/bin/toybox` is endowed only what its own
row grants, through a `[programs]` row the build declares over the existing
binary keyed by the invoked path, and the shipped image's applets are held to
their policy as the test image's are.
