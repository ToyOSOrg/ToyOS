---
status: open
kind: defect
opened: 2026-09-28
---

# `userdev_dma_fault` forbids the panic netd answers a refused claim with, so it reds whenever netd reaches its loop

The orchestrator's nightly for PR #562 at `925e1a66`, where the test still
carried `handle_basic`:

```
FAIL userdev_dma_fault: "panicked at" on a boot console that should not have it: "{0.458 error netd} thread 'main' (1) panicked at netd/src/main.rs:186:35:"
```

The boot's capture, in order:

- `spawn: /system/bin/netd pid=4` at 0.419;
- `iommu: DMA FAULT owner=slot0 … stream=00:03.0 … bme=cleared` at 0.436;
- netd's `netd: this NIC's claim refused an interrupt read: Io`, from
  `Card::begin_pass`, at 0.458;
- `pcidev: PCI 00:03.0 [1af4:1041] released from slot 0` at 0.528;
- `exit: netd pid=4 code=101` at 0.532.

The same head's Fast tier ran the test green. It is the one red among 39
unmutated runs in the orchestrator's kept logs.

That panic is netd's designed answer. `Card::begin_pass`
(`userland/netd/src/main.rs`) panics when the claim refuses its interrupt
read for anything but `WouldBlock`. After a fault on a stream a process
drives, the kernel refuses every later call on that claim.
`userdev_dma_fault` (`tests/common/iommu.rs`) stages exactly that fault and
then requires the console clean apart from the fault line. So it passes only
on a boot where netd has not reached its loop by the time the capture
closes, which is the case
`issues/kernel/netd-never-reaches-its-loop-under-iommu-userdev-foreign-dma.md`
records. This capture also carries userland lines on that boot's console,
netd's and `test-runner`'s.

It is not PR #562's. That branch changes neither netd nor logd, and its
capture for this test is main's without two flat drains, so it can only see
fewer lines than main's. It is not #571's either: the red ran
`handle_basic`, before #571 moved the test to `log_origin`.

**Exit**: the test and netd agree on what follows the fault. The test waits
for netd's own end after the fault and requires the line it dies with,
rather than forbidding it. A boot where netd never reaches its loop is then
red by name, not green. Owner: `userdev_dma_fault` in `tests/common/iommu.rs`,
with `Card::begin_pass` in `userland/netd/src/main.rs`; nobody holds it.
