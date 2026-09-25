---
status: open
kind: defect
opened: 2026-09-25
---

# The PCH MAC's bring-up clears 128 multicast table dwords where it has 32

`toyos-i219`'s `I219::open` zeroes `regs::MTA_DWORDS` (128) dwords from
`MTA` on either part. That is the 82574's table (§10.2.5.21). The PCH MAC's
is 32 dwords (`regs::MTA_DWORDS_PCH`, stated from the I219 datasheet's PHY
copy and Linux's `ich8lan.c`, `mta_reg_count = 32`), so on the T14 the
bring-up writes zero to 96 dwords past its table, at offsets `0x5280`–`0x53FC`
whose meaning on that part nothing here documents.

## Exit condition

The bring-up clears the table the part has and nothing past it, with the
driver's model refusing a write past the PCH's table on `Part::I219`.
