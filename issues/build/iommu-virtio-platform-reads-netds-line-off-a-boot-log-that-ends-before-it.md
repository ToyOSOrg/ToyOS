---
status: open
kind: tooling
opened: 2026-09-30
---

# `iommu_virtio_platform` reads netd's line off a boot log that ends before it

Red in two whole runs on the dev host under load, on two branches, with
`"netd: this claim answers 4096 bytes …" never reached the boot console`, and
green alone after each. Green on the five nightlies 36400924827, 36496779560,
36550208853, 36600425263 and 36696295750.

The arm with a unit reads that line with `Serial::boot`, whose text ends at
the boot's ready marker, and on `tests/netcase` the marker is test-runner's
`===READY===`, the first thing test-runner prints. netd prints the line from
`userland/netd/src/virtio_net.rs` only once it has claimed the NIC and walked
its configuration space. init starts netd before test-runner and nothing
orders the two lines, so a netd slower than test-runner reds the arm.

**The test is deleted**, as a flaky test is: `c34586b64` took it out, and
`git revert c34586b64` brings it back as it stood before #536;
`git show 84471bc58:tests/common/iommu.rs` holds #536's adaptation of its
no-unit arm. `issues/kernel/a-machine-without-an-iommu-refuses-every-claim.md`
and `issues/kernel/a-claims-own-refusals-are-read-by-nothing.md` name its arms
as their instrument.

**Exit**: netd's line is waited for on the guest's own liveness rather than
read off the boot log, and the test is restored and green beside other
guests.
