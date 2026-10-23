---
status: open
kind: defect
opened: 2026-10-22
---

# A metal row names no machine, so another bench's boot carries it

`tests/common/metal.rs`'s `Metal` is its arms and its judge, and nothing says
which bench a row is for. A `boot:<image>` selection collects every row whose
arms ride that boot, whichever bench the run is on. On 2026-10-22,
`amdvi_usbd_drives_its_own_entry`, an AMD-Vi row, rode the T14's `testcases`
boot. Its deliberate DMA fault (`test_rs_usbd_spare_fault`, aimed at
`0x50200000`) then failed every judge of that boot that requires a clean
kernel log, `klogd_hosted` among them. The AMD row's own judge, which accounts
for that fault, never ran there.

The stick's controller is no longer pinned per machine (the `@boot` place), so
which spare a row claims is decided at boot. Which bench a row runs on is still
not declared, and the harness has nothing to refuse it by.

**Exit**: every metal row declares the machines it runs on, by the SMBIOS
identity the machine's record carries. A selection run on one bench takes no
row that is not declared for it, and says which rows it left out.
