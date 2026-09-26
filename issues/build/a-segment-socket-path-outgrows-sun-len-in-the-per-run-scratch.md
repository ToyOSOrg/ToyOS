---
status: open
kind: tooling
opened: 2026-09-26
---

# A segment's socket path outgrows `SUN_LEN` in the per-run scratch

Since #529 (`e48604c0`) a test lane lives under a per-process scratch
directory, and `tests/common/segment.rs` names each guest's tap socket
inside the lane: on the dev host,
`/private/var/folders/gr/mr4_fg4n34jb417sx1g5cgxc0000gp/T/toyos-tmp-65213-0/tests-0/lane-8/tap-out-0.sock`
is 104 bytes. macOS's `sun_path` holds 104 bytes including the terminating
NUL, so the path does not fit. `lan_mdns_answer` fails, and fails again when
re-run alone:

```
FAIL lan_mdns_answer: connect to QEMU's /private/var/folders/gr/mr4_fg4n34jb417sx1g5cgxc0000gp/T/toyos-tmp-65213-0/tests-0/lane-8/tap-out-0.sock: path must be shorter than SUN_LEN
```

Seen in the fast tier on `wt/toyos-logtrack` right after it merged
`e48604c0`. Nothing in that branch touches the scratch paths or
`segment.rs`. `cargo run -- --known-red lan_mdns_answer` answers that it is
not quarantined. Whether a lane is red depends on the lane number, the pid
and the host's `TMPDIR`, so the same test is green on a shorter path.

## Exit condition

A tap socket's path is bounded below `SUN_LEN` whatever the pid, lane and
`TMPDIR` — for example a short socket directory, or a path relative to a
directory the harness `chdir`s QEMU into. Shown by `lan_mdns_answer` green on
the dev host from a lane numbered 10 or more.
