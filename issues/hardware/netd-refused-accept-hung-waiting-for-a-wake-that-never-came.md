---
status: expected-red
kind: defect
opened: 2026-09-28
---

# `netd_refused_accept` hung waiting for a wake that never came

Orchestrator Fast tier at PR #562's head `2a9c77ee`: `FAIL netd_refused_accept:
timed out after 2341s, with the guest still talking 8s ago`. The guest's last
line was `netd_refused_accept: waiting for a wake for the connection an accept
refused for room left, once room returned` — the `wake()` call at
`tests/toyos-rust-tests/src/bin/netd_refused_accept.rs:63`, blocked reading
`listener.notify`. #562 changes nothing this test runs: the guest binary,
netd, the netcase config and the harness function that drives it are
identical to main.

Unconfirmed reading, not established from a kept console: the same run's
`the host's connections ended [Ok(585728), Ok(0), Ok(0), ...]` shows the
host's dial ending `Ok(585728)` rather than with the harness's 120 s
write-stall error. A reset would return the listener's socket to `Listen`
(`settle` in `userland/netd/src/listen.rs:73-85`, the `tcp::State::Closed`
arm re-`listen`s without producing an accept), after which netd owes the
test's `end(held.pop()...)` connection no wake. The guest console for this
run was not kept, so which path the reset actually took is not established.

Exit condition: the mechanism established from a kept console, fixed, and
`netd_refused_accept` green — which needs `netcase_against_host`
(`tests/toyos.rs:10113-10121`) to keep `result.serial` on its error path
instead of dropping it, since nothing else in the tree keeps this test's
console.

**Owner**: whoever holds `issues/design-debt/toyos-has-its-own-network-stack.md`.
