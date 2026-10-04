---
status: open
kind: tooling
opened: 2026-10-01
---

# A host's apps are judged on other hosts' runners

`cargo run -- --ci host` builds each host's apps where it runs on that host's
triple, and checks them against it elsewhere (`src/userlandhost.rs`). `ci.yml`
runs it on Linux alone and the nightly on Linux and macOS, so on a pull
request macOS's and Windows's apps are only checked, and Windows's are built
nowhere. These go unseen:

- on a host whose apps are only checked, a link failure, and an error only code
  generation raises;
- a build script that answers differently on the host it is judged for: one
  that runs `cc` or `pkg-config` probes the host that checks, so an app that
  compiles C or links a system library cannot pass a check of another host's
  triple;
- a declared failure that outlives its fix. A host an app's `fails` names is
  attempted on no runner, so the declaration keeps that host unjudged while it
  claims the app fails there. Attempting it, red when the app builds, is sound
  only where one runner judges the host: while two do, a fix that passes on one
  and fails on the other is red whether its `fails` entry stays or goes.

Owner: the host gate, `src/userlandhost.rs` and `src/ci.rs`'s `apps_for`.

**Exit:** on each pull request, a runner of each host builds that host's apps,
judges no other host's, and attempts each app whose `fails` names its host, red
when one builds. Windows waits on
`issues/the-build-system-does-not-compile-on-windows.md`, macOS on a
macOS runner in `ci.yml`.
