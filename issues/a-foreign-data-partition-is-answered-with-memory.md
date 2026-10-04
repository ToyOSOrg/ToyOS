---
status: open
kind: defect
opened: 2026-09-28
---

# A foreign DATA partition is answered with memory

`userland/fsd/src/main.rs`'s `data_on` answers `Probed::Foreign` — a
TOYOS-DATA partition that holds no volume of ours and no designation stamp —
with `ram()`: `/apps`, `/config`, `/home` and `/state` are served from memory,
and everything written under them is lost at the next reboot. That is a disk
that is there and cannot be used, answered as though the machine had none,
which is the harm `fsd::absent::Absent` exists to refuse; a refused NVMe
claim and two DATA partitions are refused by name for the same reason.

`foreign_disk_untouched` (`tests/common/storage.rs`) boots this arm and
asserts the partition is not written and the `no volume of ours` line, and
nothing about what stands in for it.

**Exit condition:** a foreign DATA partition serves DATA absent by name, and
`foreign_disk_untouched` asserts fsd's absent line and that the in-memory line
is never said.
