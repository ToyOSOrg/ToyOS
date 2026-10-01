---
status: expected-red
kind: tooling
opened: 2026-10-01
---

# `iommu_virtio_platform` reads netd's lines off a boot log that ends before them

Red on four branches that do not touch it, in two words of one race:

- `650-libcllvm-whole.log` (`wt/toyos-libcllvm`, libc only) and
  `634r2-whole.log` (`wt/toyos-sk6` `fb0fc7b56`): `"netd: this claim answers
  4096 bytes of configuration space and refuses every access outside them"
  never reached the boot console`.
- `637r2-whole-suite.log` (`wt/toyos-libcxx` `26a4bfbe1`) and
  `641f-r3-whole.log` (`wt/toyos-tonefix` `597d60a4e`): `QEMU created 3 virtio
  function(s) … and the guest negotiated features with 2`, the missing one
  being netd's own function.

`tests/common/iommu.rs`'s `iommu_virtio_platform` judges `Serial::boot`, the
capture `wait_for_ready` ends at test-runner's `===READY===`. init starts netd
before test-runner, and netd's claim and its feature negotiation come after
netd starts, so test-runner's marker can come first: in the 650 run netd
started at 4.905 and the marker came at 5.098 with neither of netd's lines
before it.

`wt/toyos-noredlist` (#639) carries a fix, `d773a4306`: each arm waits on the
guest for the daemons' lines it reads.

**Exit**: the test waits for netd's lines rather than reading them off the
boot log; then the row goes.
