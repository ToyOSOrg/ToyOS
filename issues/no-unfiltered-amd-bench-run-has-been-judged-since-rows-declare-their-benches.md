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

Owner: the metal harness (`tests/common/metal.rs`, the `METAL` table in
`tests/toyos.rs`); the benches are the orchestrator's.

## Exit condition

One unfiltered `cargo test --test toyos-build -- --metal --metal-bench amd`
run, judged: each red is a defect of ToyOS with its own record, or a row or
member the AMD bench cannot run, declared so.
