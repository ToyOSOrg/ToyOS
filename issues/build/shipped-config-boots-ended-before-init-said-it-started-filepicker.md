---
status: expected-red
kind: tooling
opened: 2026-09-27
---

# `shipped_config_boots` ended before init said it started filepicker

PR #542's nightly at 059c5de7 (run 36328646395, `guest (12)`), KVM, QEMU
11.1.0, one guest on the runner:

```
FAIL shipped_config_boots: the shipped `[boot] start` names filepicker and init never said "init: started filepicker"
```

The capture's last lines are `{0.852 netd} netd: ready, …` and then the
kernel's `spawn: /system/bin/filepicker pid=8` at 0.887 s; nothing follows it.
Every other `[boot] start` program's `init: started` line is in the capture.
The branch changes no kernel, guest or `system.toml` code. The test was green
at a4f68c5a (run 36314576406, `guest (3)`), at 16d2e645 (run 36306830048,
`guest (3)`) and at 1ce71831 (run 36290616312, `guest (3)`).

By reading, not reproduced: `shipped_config_boots` in `tests/toyos.rs` waits
for the four daemons' markers with `qemu::await_marker`, and then checks each
`init: started <program>` against the log read so far without waiting for it.
filepicker is started after netd, so `netd: ready` arriving last lets the check
run before init's line about filepicker can reach the console.

**Exit condition.** Re-enabled when the test waits for every `init: started
<program>` line it asserts, and a boot that holds filepicker's start back past
the daemons' markers is shown green. Owner: `shipped_config_boots` in
`tests/toyos.rs`; nobody is holding it yet.
