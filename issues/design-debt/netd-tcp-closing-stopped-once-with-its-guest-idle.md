---
status: open
kind: defect
opened: 2026-09-26
---

# `netd_tcp_closing` stopped once with its guest idle

`cargo test --test toyos-build -- --nightly netd_` on the dev host, 12 wide,
TCG, load average about 30, at `ce0bb621`: `netd_tcp_closing` — ninety-six
connects, each written to and dropped — stopped after some of its connections
had reached the host (their `Hold` records came back), and its QEMU used 0.1%
of a CPU for the 21 minutes before it was killed by hand, so neither netd nor
the test was running: something waited on a wake that never came. It passed
alone in the same session, and in two wide runs of every fast `netd_tcp_` test
after it, and in every earlier run of the branch.

The guest's console after boot goes to the virtio console, which the capture
did not keep, so no waiter is named. `issues/kernel/a-shared-boot-stopped-answering-and-no-capture-says-why.md`
records the same shape on other tests.

Exit condition: a capture of a stopped `netd_tcp_closing` boot that names the
waiter and what it waits on, or the defect fixed where that capture points.
