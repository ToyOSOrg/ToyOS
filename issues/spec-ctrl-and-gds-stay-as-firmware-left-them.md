---
status: open
kind: defect
severity: [security]
opened: 2026-09-29
---

# SPEC_CTRL and GDS stay as firmware left them

**Built**: each CPU's bring-up writes `IA32_SPEC_CTRL` (0x48) whole with
what
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`
names for it while no program that asked runs: `IBRS` for enhanced IBRS or
AMD's always-on IBRS, `BHI_DIS_S` where it is named, every other bit clear.
It clears each `IA32_MCU_OPT_CTRL` (0x123) opt-out, `RNGDS_MITG_DIS` and
`GDS_MITG_DIS`, whose microcode mitigation the decision names
(`kernel::mitigation::Decision::spec_ctrl`, `mcu_opt_ctrl_clears`). Each
boot's words line gives both registers as each CPU found them and as it left
them, and its lines take what is in force from what it read back.
`checks::the_benches_words_decide_as_their_vendors_rows` holds the T14's
lines with `IBRS` set, and the `mitigations` row refuses a boot whose
registers leave off anything the kernel sets
(`Decision::unapplied`). The `gds-found-disabled` arm sets `GDS_MITG_DIS`
before the kernel's write, as firmware that opted out would leave it, and the
`gds_found_disabled` row holds that every CPU read it set and left it clear.

**Left**:
- **Owed on metal:** the `mitigations` row, the `metalcase` boot of main.
  - Benches: the T14 and the AMD bench.
  - Main must show the row green: on the T14 every CPU's words line with
    `spec_ctrl=0x1` and `mcu_opt_ctrl` with bits 0 and 4 clear, and on the
    AMD bench `spec_ctrl=0x0`.
- **Owed on metal:** `gds_found_disabled`, the `shared-debug` boot of main.
  - Bench: the T14.
  - Main must show the row green: every CPU's words line with
    `mcu_opt_ctrl_found` holding bit 4 and `mcu_opt_ctrl` without it. The
    same boot of main with the clear deleted from `arch::mitigation::apply`
    must red it, the mutation below.
- **Owed on metal:** Linux's reads of 0x48 and 0x123 on each bench, beside
  ToyOS's, compared in evidence.

**Exit**: on every proving machine each CPU's 0x48 holds what the decision
selects for it; on the T14, the one proving machine with GDS, a probe sets
`GDS_MITG_DIS` and reads it clear after the kernel's write. **Mutation**: the
clear deleted. **Oracle**: the vendor's guidance through that function, and
ToyOS's own reads on each machine. Linux's reads of 0x48 and 0x123 on each
machine are a goal compared in evidence, never a test's expected value.
