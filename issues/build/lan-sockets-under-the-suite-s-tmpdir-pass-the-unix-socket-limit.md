---
status: open
kind: tooling
opened: 2026-09-26
---

# The LAN tests' sockets under the suite's `$TMPDIR` pass the Unix socket limit

Since #529 (`e48604c0`) a suite's scratch lives under a per-run directory in
`$TMPDIR`, and `tests/common/segment.rs` makes each lane's socket there
(`tap-in-{n}.sock`, `tap-out-{n}.sock`). On macOS `$TMPDIR` is
`/private/var/folders/<2>/<28>/T/`, and the result is 104 bytes:

```
/private/var/folders/gr/mr4_fg4n34jb417sx1g5cgxc0000gp/T/toyos-tmp-47355-0/tests-0/lane-0/tap-out-0.sock
```

`sockaddr_un.sun_path` holds 104 bytes with its terminator, so the connect
fails (`path must be shorter than SUN_LEN`), and `lan_mdns_answer` reds before
the lane carries a frame. Measured on PR #524's A/B: `main` at `e48604c0`
red 4 of 4 runs, the branch 5 of 5, every one on this sentence; the fast tier
at the branch's merge of `e48604c0` red on it too. The wide run's first
failure, `QEMU died before ===READY=== (status … 256)`, is the other end of the
same socket. The pid in the path also makes the harness's `ALONE` line call the
two readings "a DIFFERENT failure".

**Exit condition**: every socket the harness makes has a path the platform's
`sun_path` holds, checked where the path is made rather than at `connect`, and
`lan_mdns_answer` is green again on the dev host.
