---
status: open
kind: tooling
opened: 2026-10-01
---

# A test asserts a daemon's line off a boot log that ends before it

`QemuInstance::boot_log` ends at test-runner's `===READY===`, and init starts the daemons before
test-runner, so a daemon's line can come after the marker. Two tests read one from it:

- `iommu_virtio_platform`, red on four branches that do not touch it: `650-libcllvm-whole.log`
  (`wt/toyos-libcllvm`) and `634r2-whole.log` (`wt/toyos-sk6` `fb0fc7b56`), `"netd: this claim
  answers 4096 bytes of configuration space and refuses every access outside them" never reached
  the boot console`; `637r2-whole-suite.log` (`wt/toyos-libcxx` `26a4bfbe1`) and
  `641f-r3-whole.log` (`wt/toyos-tonefix` `597d60a4e`), `QEMU created 3 virtio function(s) … and
  the guest negotiated features with 2`, the missing one netd's own. In the 650 run netd started
  at 4.905 s and the marker came at 5.098 s with neither of netd's lines before it.
- `lan_dhcp_lease`, red in `638L-638r5-whole.log` (`wt/toyos-tight` `59940c452`): `"netd: ready,
  at most " never reached the the lan boot after "netd: DHCP: lease "`. It awaits the lease line,
  which the boot log already carries, so the wait returns at once and the capture is whatever
  arrived before `===READY===`:

  ```
  {1.281 netd} netd: DHCP: lease 10.0.2.15/24 from 10.0.2.2, gateway 10.0.2.2, dns [10.0.2.3], 43 ms after netd came up
  {1.290 test-runner} ===READY===
  ```

`wt/toyos-noredlist` (#639) has each `iommu_virtio_platform` arm wait on the guest for the
daemon's lines it reads (`d773a4306`). `lan_dhcp_lease` has left the guest suite: its T14 row
judges the whole readback, which ends at the boot's last word.

Owner: the orchestrator.

**Exit**: no guest test reads a daemon's line out of `boot_log()` without waiting on the guest
for it, and `iommu_virtio_platform` passes in a whole-suite run beside other worktrees' builds.
