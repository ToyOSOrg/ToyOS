---
status: open
kind: defect
opened: 2026-09-26
---

# A netd exit can read to its clients as a clean EOF

A client's stream ends as a reset only because netd closes the client's send
pipe before its receive pipe reaches its end (`userland/netd/src/stream.rs`,
`End::Reset`), and std's pal asks the send pipe at every EOF
(`library/std/src/sys/net/connection/toyos.rs` in the fork, `ended`). When
netd itself exits — a crash, a panic, a swap — nothing orders that: the kernel
closes netd's handles in whatever order it tears them down, and a client whose
receive pipe happens to close first reads the end of a stream that did not
end, as though its peer had sent its FIN. An upload whose answer is read that
way is taken as complete.

Read from the teardown path; not reproduced by a test.

Exit condition: a client reading from a stream when netd exits is told the
stream did not end in order, with a test that kills netd mid-download.
