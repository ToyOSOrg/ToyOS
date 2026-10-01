---
status: open
kind: tooling
opened: 2026-10-01
---

# No app is built for a host: each is only checked

`cargo run -- --ci host` judges every app the images ship for Linux, macOS and
Windows by `cargo check --target <triple>`, the same on whichever host runs it
(`src/userlandhost.rs`). A check links nothing and runs build scripts on the
host that checks, so on every host these go unseen:

- a link failure, and an error only code generation raises;
- a build script that answers differently on the host it is judged for: one
  that runs `cc` or `pkg-config` probes the host that checks, so an app that
  compiles C or links a system library cannot pass a check of another host's
  triple.

A `cargo build` where the host is the one judged would give that host a second
verdict beside the check another host makes of it.

Owner: the host gate, `src/userlandhost.rs`.

**Exit:** each host's apps are built, `cargo build`, on a runner of that host on
each pull request, and no host checks another's. Windows waits on
`issues/build/the-build-system-does-not-compile-on-windows.md`, macOS on a
macOS runner in `ci.yml`.
