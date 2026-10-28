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

The T14's `00:0d.0` [8086:9a13] is a spare under both boot layouts. Off the
stick, `00:14.0` is the boot controller. Off the NVMe (#1095), no xHCI is. The
AMD row claims the spare and its fault arm runs:
`iommu: DMA FAULT owner=process unit3 stream=00:0d.0 addr=0x0000000050200000`,
1 GiB past the grant at `0x10200000`. `klogd_hosted` then reds on
`must_be_clean`. The fault ran on each of PR #1074's four stick boots
(sessions 2–5), and `klogd_hosted` red on every one judged (sessions 2–4;
[sessions 2 and 3](https://github.com/ToyOSOrg/evidence/tree/b1c036144b4c8a39415cb60145a823d0c0d2d652/r3)).
The same happened on its NVMe landing boot (head 094323f29,
[evidence](https://github.com/ToyOSOrg/evidence/tree/3cb575aadca9e85f33b4c5690c7e1ea4ca15822f/r8)).

Four more rows red on that boot by this record's mechanism, because they are
AMD-bench rows judged against a T14 readback:
- `amdvi_usbd_drives_its_own_entry`: `test_rs_usbd_spare_vector` finds no
  MSI-X on the T14's MSI-only xHCI;
- `idle_entered`: "this row runs on the AMD bench";
- `amdvi_translates_the_boot_controller` and
  `amdvi_remaps_the_boot_controller`: no AMD-Vi unit is translating.

The boot's two other reds have records of their own:
`i219_messages_on_its_msi_vector` (`issues/i219-msi-passes-only-when-the-network-sends-it-a-frame.md`)
and `hda_tone` (`issues/the-t14-tone-underran-one-period-once-in-nine-boots.md`).

**Owner**: the metal harness (`tests/common/metal.rs`, and the row table in
`tests/toyos.rs`).

**Exit**: every metal row declares the machines it runs on, by the SMBIOS
identity the machine's record carries. A selection run on one bench takes no
row that is not declared for it, and says which rows it left out.
