---
status: open
kind: defect
opened: 2026-08-27
---

# The harness's own guest programs cannot appear in a `system.toml`, so they hold nothing — and that is what `desktop_window_child` has been failing on

`tests/toyos-rust-tests`' binaries reach an image as **extra files**:
`qemu.rs`'s image builder pushes each one in as `bin/test_rs_<name>` beside the
initrd's real programs. Nothing else happens to them. `/bin/init` builds a
child's namespace and device claims from the manifest, and a name the manifest
does not carry is a name it endows with nothing.

**A row cannot be added either.** `build_and_assemble` asserts that every
`[programs.<name>]` names a crate — `userland/<name>/Cargo.toml`, or the
directory an explicit `path` gives — so declaring one is:

```
Program 'test_rs_window_child' crate not found at …/userland/test_rs_window_child
```

Measured 2026-08-27: three runs, all `FAIL … (1ms)`, which is the build
refusing before any guest boots. So the endowment a harness-injected program
needs has no spelling at all.

## What it is currently costing

`desktop_window_child` — the one QEMU reproduction anybody has of #156
(`issues/kernel/desktop-window-child-freeze.md`) — stops at its **first** probe
and has done since the capability-endowment work landed. Its client asks for a
window, is answered `NotEndowed`, prints `WINDOW-CHILD-REFUSED this program was
given no compositor` and exits `code=1` eight milliseconds after it is spawned.
The harness renders that as `the windowed child never reported leaving`, which
is one of the six messages that entry's `EXPECTED_FAILURES` declaration
absorbs — so the test has been red *for a reason nobody chose* under a
declaration written about something else, and no run says so.

Measured 2026-08-27, four runs on `wt/toyos-freeze` and one on `origin/main` at
`16c05999`: identical in all five, `WINDOW-CHILD-REFUSED` in every capture,
`8/8 cpu(s) answered` in every dump. It is not intermittent and it is not the
freeze.

The config says what it lost. `tests/desktopcase/system.toml` still carries
`[programs.snake] receives = ["compositor"]` from the day the test was written,
when the manifest was a flat list of names and `desktop_window_child`'s own
client was one of them. Endowment chunk 5 turned that list into declared
authority; the client was an extra file by then and had no row to convert.

## What a fix owes

Not a row for one name — a way for the harness's programs to be endowed at all,
and it is a decision rather than an implementation:

- **The manifest gains a program kind that is a file rather than a crate**, so a
  boot config can name `test_rs_window_child` and give it `receives`. That is a
  `toyos-manifest` schema change with `src/build.rs`'s gates behind it.
- **Or the endowment travels with the spawn**: a shell that holds a compositor
  connection passes one on, and `[programs.shell]` in a desktop config receives
  `compositor`. That is the capability model answering the question rather than
  the build system, and it is the shape the root `CLAUDE.md`'s "a process holds
  exactly what its parent moved into it" already describes.

Whichever it is, the gate this wants beside it is the one the tests' own
`CLAUDE.md` names as the worst defect class: an arm that does nothing, a run
that is green, and every negative control staged through it proving nothing.
A boot config that names a program the image does not carry is refused; a
program the image carries and the manifest does not name is not.
