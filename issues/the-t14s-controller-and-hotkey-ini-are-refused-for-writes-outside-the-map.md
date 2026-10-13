---
status: open
kind: defect
opened: 2026-10-13
---

# The T14's controller `_INI` and hotkey `_INI` are refused for writes outside the map

On the T14 the server's namespace walk has two `_INI`s refused `MemoryType`,
"a SystemMemory write the kernel refused MemoryType, in unlisted firmware
memory": the embedded controller's (`\_SB.PC00.LPCB.EC._INI`) and its
hotkey device's (`EC.HKEY._INI`). The kernel refuses every write the claim's
holder asks at an address the firmware's memory map does not list
(`toyos_userbound::firmware`).

**Ruled** (owner, 2026-10-12, #875): a memory write outside every range the
map describes is refused by name and recorded, not admitted. The T14 row
expects the refusal (`T14` in `tests/toyos.rs`).

**The controller's write**, read on the T14 at #875's `639ce1374` and
reproduced on the host by the T14's own tables (`ToyOSOrg/evidence`
`pr-875`, `round5/`): `EC._INI` calls `HKEY.WGIN`, which stores what the
firmware's SMI answers for `WGSV(1)` and, where `WIN8` is set and that
answer is not zero, calls `BPWC(1)`; `BPWC` calls `\_SB.SGOV(0x0902000D, 1)`
where the answer has bit 4 set and bit 7 clear. That is byte 0x01 at
0xFD6E0A70: `\SBRG` (SBREG_BAR, 0xFD000000) + Port ID 0x6E << 16 + 0xA70,
which Intel's 500 Series Chipset On-Package PCH datasheet vol. 2
(631120-002) §27.1.114 names `PAD_CFG_DW0_GPPC_A_13` of GPIO Community 0,
bit 0 `GPIOTXSTATE`, "drive a level '1' to the TX output pad". The window
0xFD6E0000 + 64 KiB is one of four the GPIO controller `\_SB.GPI0`
(`INT34C5`) lists in its `_CRS`, and each holds every pad's configuration
of its community with `PAD_OWN`, `PADCFGLOCK`, `HOSTSW_OWN` and the GPE
group assignment (§27.1.1–27.1.19); `PAD_OWN` 00 is "no read/write
restriction to the Pad Configuration register set during host ownership".

**The hotkey device's write** is not read on the T14: the server details a
kind's first refusal only. On the host the T14's tables reach it by
`HKEY._INI` → `DYTC` → `SCPF` → `\_SB.PC00.PL1S` → `CTCN`, which writes
bytes at MCHBAR + 0x5F50, 0x5F54, 0x59A0 and 0x59A1, where MCHBAR is what
00:00.0 offset 0x48 holds (the host run assumed 0xFEDC0000). Intel's 11th
Generation Core processor datasheet vol. 2 (631122) is the document for
those offsets; it is fetched and unread, its PDF encrypted against the text
extractor tried.

**What it costs**:

- `EC._INI` is refused inside `WGIN`, so the pad stays as the firmware
  left it, `WGIN`'s flags miss bit 5, and nothing after `WGIN` runs: on
  the host, under the values that run answered, that is a read of the
  controller's bytes 0x49 and 0x3A and a write of 0x3A with bit 5 set;
- `HKEY._INI` is refused inside `DYTC`, and whatever comes after its
  refused write is not done;
- the battery and the adapter are read regardless (`battery_read`).

Owner: the AML track
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`).

## Exit condition

The hotkey device's refused write is read on the T14, and either a design
the owner accepts mediates these writes by Intel's register maps, built,
with the T14's `acpi_tables_loaded` reading both `_INI`s run and the
expectation in `tests/toyos.rs` moved to it, or the refusal stays and the
row expects it.
