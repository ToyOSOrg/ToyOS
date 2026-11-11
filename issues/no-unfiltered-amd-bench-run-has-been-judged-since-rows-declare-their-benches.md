---
status: open
kind: tooling
opened: 2026-10-11
---

# No unfiltered AMD-bench run has been judged since rows declare their benches

A `METAL` row in `tests/toyos.rs` declares the benches it runs on
(`metal::On`), and a run takes no row its bench does not run. The rows held
off the AMD bench so far are those whose code names hardware or tables it
has not: the I219's, the T14's embedded controller and battery, its
hardware-managed P-states, its Tiger Lake GPU, its VT-d unit's message
format, and the T14's own `SMI_CMD` values in `acpi_server_death`. Every other
row is declared for both benches because nothing in its code says otherwise,
not because an AMD-bench run passed it: the shared members ride both benches
undeclared, and several rows have no AMD-bench verdict in any judged run
(`kernel_objects_refused`, `memory_floor`, `gop_edid_scale`, `ps2server_arms`,
`msix_table_read_in_place`, `domains_map_clear_of_the_host_bridges`,
`isa_claim_refused_on_a_kernel_port` among them).

- **Owed on metal:** no run has judged the bench declarations themselves.
  The rows are `boot:testcases` (`testcases` and its `testcases-watchdog`
  companion), staged with `--metal-bench t14` and with `--metal-bench amd`.
  They run on both benches and are judged with neither `--metal-bench` nor
  `--metal-host` on the judge's line. Main must show:
  - the run's opening, boot and timings lines name the bench its readbacks
    name, and `request.txt`'s invocations carry that bench's `--host`;
  - each image's job list equals main's `--metal --list --metal-bench` plan
    for that bench;
  - on the T14:
    - the `testcases` log carries none of `test_rs_usbd_spare_fault`,
      `test_rs_usbd_spare_vector` or `test_rs_idle_entered`, and no
      `iommu: DMA FAULT owner=process`;
    - no red comes from the rows the T14 once carried
      (`console_uart_driven`, `idle_entered`, `amdvi_*`), and every red that
      remains has its own record;
  - on the AMD bench:
    - the `testcases` log carries none of `test_rs_pci_reclaim`,
      `test_rs_i219_msi`, `test_rs_counters_metal` or `test_rs_acpi_counted`;
    - no I219 row is red, a failed job reads "on the AMD bench", and its reds
      are this record's first slice;
  - `loader_watchdog_arms` is judged across both boots on each bench.

  The AMD bench booted both images on 2026-11-11, staged at the declarations'
  own pull request (#1254), and its loop passed both. Their readbacks were
  deleted before a judge read them, so neither bench's run has been judged.

Owner: the metal harness (`tests/common/metal.rs`, the `METAL` table in
`tests/toyos.rs`); the benches are the orchestrator's.

## Exit condition

One unfiltered `cargo test --test toyos-build -- --metal --metal-bench amd`
run, judged: each red is a defect of ToyOS with its own record, or a row or
member the AMD bench cannot run, declared so.
