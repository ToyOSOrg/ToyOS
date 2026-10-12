---
status: open
kind: track
opened: 2026-09-29
---

# blockd drives NVMe APST

After blockd drives the T14's drive, the blockd step of
`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`. The NVMe
base specification defines APST: enabled through Feature 0Ch, the controller
moves itself, once idle for the time a power state's table entry names, to
the non-operational state that entry names, one with no higher idle power;
and the drive reports each state's worst entry and exit latency, a
transition costing the old state's exit latency plus the new one's entry
latency (NVM Express Base Specification 2.1, §5.1.25.1.6, §8.1.17 and
§8.1.17.2). blockd builds the table from the power state descriptors of the
drive's own `id-ctrl`. The specification bounds no latency: blockd's bound is
its design round's, chosen on its merits. Drives whose APST failed are
recorded, and blockd honours the record: no APST on a drive marked
`NVME_QUIRK_NO_APST`, and no transition to its deepest state on one marked
`NVME_QUIRK_NO_DEEPEST_PS`, by the drive, firmware or machine Linux lists for
each (`drivers/nvme/host/core.c`'s `core_quirks`, and `pci.c`'s
`nvme_id_table` and `check_vendor_combination_bug`, at v7.2). Whether the
T14's drive is one of them is unread: no reading in the tree names its PCI ID,
model or firmware, and the program of
`issues/no-program-measures-toyos-against-linux-on-one-machine.md` reads its
NVMe identity.

**Exit**: host tests of the APST table, a quirked drive's among them; on the
T14, the one proving machine with a physical NVMe drive, the block figures and
the idle minute with the drive idle. **Mutation**: a table that admits a state
past the bound. **Oracle**: the specification's rules for the table over the
T14's `id-ctrl`.
