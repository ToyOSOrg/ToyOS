---
status: open
kind: defect
opened: 2026-10-09
---

# The SMMUv3 and ITS crates declare what the AArch64 kernel also declares

`toyos-smmu` and `toyos-its` came in with no kernel file touched, so each
declaration below has a second home in `kernel/`, and nothing holds the two
equal. `toyos-smmu` takes the stage 1 descriptors, the `MAIR` and `PXN` from
`toyos-bootmap::aarch64` already; what is left is what `toyos-bootmap` does not
declare.

**The stage 1 descriptor.** `toyos-smmu/src/table.rs` declares `VALID` (bit
0), `TABLE_OR_PAGE` (bit 1), `AP_UNPRIVILEGED` (bit 6), `AP_READ_ONLY` (bit
7), `NOT_GLOBAL` (bit 11) and the level index `at >> (39 - 9 * level) & 0x1FF`.
`kernel/src/arch/aarch64/paging.rs` declares the same bits as `VALID`,
`TABLE`, `AP_EL0`, `AP_READ_ONLY` and `NOT_GLOBAL`, and the same `index`. It
also declares `INNER_SHAREABLE`, `OUTER_SHAREABLE`, `AF`, `PXN` and `UXN`,
which `toyos-bootmap/src/aarch64.rs` holds: the kernel's own copy of that
crate's.

**The redistributor.** `toyos-its/src/lpi.rs` declares `GICR_CTLR.EnableLPIs`,
`GICR_TYPER.PLPIS` and `GICR_TYPER.Processor_Number`. `GICR_TYPER`'s other
fields (`VLPIS`, `Last`, the affinity) are `kernel/gicv3/src/lib.rs`'s, and
the two registers' offsets are `kernel/src/arch/aarch64/irqchip.rs`'s: one
register's fields have two crates. `toyos_its::TRANSLATION_FRAME` and
`toyos_gicv3::FRAME` are both the GIC's 64 KiB frame.

**The distributor.** `toyos-its/src/lpi.rs`'s `Layout::new` decodes
`GICD_TYPER`'s `LPIS`, `IDbits` and `num_LPIs`, and
`kernel/src/arch/aarch64/irqchip.rs`'s `route_iommu_events` decodes its
`ITLinesNumber`: one register's fields have two homes.

**Owner.** `issues/toyos-runs-on-arm64.md`. The descriptor is stage 4's, its
owed break-before-make ordering of a live entry's replacement, which rewrites
how `kernel/src/arch/aarch64/paging.rs` writes an entry. The redistributor and
the distributor are stage 6's ITS work, which edits `irqchip.rs` and reads
`GICR_TYPER` for every CPU and `GICD_TYPER` for its LPIs.

**Exit.** Each bit and the level index has one declaration that the kernel's
tables, the loader's and the unit's are all written from, and each
redistributor or distributor register's fields have one home:

```
rg -n 'const (VALID|TABLE|TABLE_OR_PAGE|AP_\w+|NOT_GLOBAL|INNER_SHAREABLE|OUTER_SHAREABLE|AF|PXN|UXN): u64' \
    kernel/src/arch/aarch64/paging.rs toyos-smmu/src/table.rs
rg -n 'fn index' kernel/src/arch/aarch64/paging.rs toyos-smmu/src/table.rs
```

print nothing, `rg -l 'GICR_TYPER' toyos-its kernel/gicv3` names one
directory, and `rg -l 'ITLinesNumber|IDbits|num_LPIs' toyos-its/src kernel/src
kernel/gicv3/src` names files of one crate.
