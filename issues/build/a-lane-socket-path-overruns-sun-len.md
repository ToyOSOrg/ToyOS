---
status: open
kind: tooling
opened: 2026-09-26
---

# A lane's tap socket path overruns macOS's `sun_path` from lane 10 on

`segment::Tap::in_lane` puts QEMU's two filter sockets in the lane's scratch
directory (`<TMPDIR>/toyos-tmp-<pid>-0/tests-0/lane-<n>/tap-out-<k>.sock`).
macOS's `sockaddr_un.sun_path` holds 104 bytes with its terminating NUL, and
on this dev host's `TMPDIR`
(`/private/var/folders/gr/mr4_fg4n34jb417sx1g5cgxc0000gp/T/`) a four-digit
pid and lane 10 make the path exactly 104 bytes, so the connect is refused
before QEMU is asked anything:

```
FAIL lan_mdns_answer: connect to QEMU's /private/var/folders/gr/mr4_fg4n34jb417sx1g5cgxc0000gp/T/toyos-tmp-6065-0/tests-0/lane-10/tap-out-0.sock: path must be shorter than SUN_LEN
```

Recorded in a fast-tier run of the `wt/toyos-update` branch at `669fbc02`
(PR #530), whose diff touches none of `tests/common/segment.rs`,
`tests/common/lane.rs` or `toyos-tmpdir`; the harness re-ran it alone, in a
low lane, green, and called its `Sched::Parallel` wrong — the lane number,
not the neighbours, is the cause. A five-digit pid or a longer `TMPDIR`
moves the threshold below lane 10.

**Exit**: a socket path this harness creates is bounded by construction — a
short directory of its own for sockets, or a path shortened to fit — and a
test holds every socket path the suite can create, at its widest lane and
pid, under `sun_path`.
