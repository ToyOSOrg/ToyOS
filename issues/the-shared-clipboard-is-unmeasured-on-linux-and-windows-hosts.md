---
status: assigned
kind: tooling
opened: 2026-10-13
---

# The shared clipboard is unmeasured on Linux and Windows hosts

Held by the orchestrator.

`cargo run` gives QEMU the host's clipboard through its cocoa display on macOS
and through GTK's, `-display gtk,clipboard=on`, on every other host
(`build/qemu.rs`). Every fact about QEMU 11.1.1's vdagent that
`/system/bin/vdagent` rests on was measured black-box through QEMU's VNC
clipboard peer (`system/vdagent/bridge/src/lib.rs`); GTK's peer and cocoa's are
unmeasured. QEMU documents GTK's clipboard as off by default
(`qapi/ui.json`), and QEMU issue #1150 reports guest hangs with it on.

**Exit condition.** A copy and a paste each way, measured on a Linux host
under GTK and on a Windows host.
