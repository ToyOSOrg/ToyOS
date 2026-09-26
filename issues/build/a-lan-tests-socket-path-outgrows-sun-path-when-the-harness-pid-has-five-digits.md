---
status: open
kind: defect
opened: 2026-09-26
---

# A LAN test's socket path outgrows `sun_path` when the harness's pid has five digits

`tests/common/segment.rs` names each guest's frame socket
`<lane dir>/tap-out-<n>.sock`, and the lane directory is under the run's
scratch, `$TMPDIR/toyos-tmp-<pid>-0/tests-0/lane-<k>`. On the dev host
`$TMPDIR` is `/var/folders/gr/<id>/T/`, so the whole path is 103 bytes with a
four-digit pid and 104 with a five-digit one — and macOS's `sun_path` holds
103 and a NUL. A fast tier whose harness ran as pid 10033 lost
`lan_mdns_answer` both times: wide, QEMU exited 1 before `===READY===` with
nothing on either console; alone, `connect to QEMU's …/tap-out-1.sock: path
must be shorter than SUN_LEN`. The same test was green in a run as pid 3345.

Which pid the harness gets decides whether every LAN test can run, and the
wide failure names nothing of why.

Exit: the socket's path is bounded by construction (a short directory of its
own, or a path relative to a directory QEMU is started in), and a harness
run under a long `$TMPDIR` is the test.
