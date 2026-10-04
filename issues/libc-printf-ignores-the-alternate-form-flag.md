---
status: open
kind: defect
opened: 2026-10-01
---

# libc's printf ignores the alternate-form flag

`printf`'s format reader (`userland/libc/src/printf.rs`) steps over `#` and
keeps nothing of it, so `%#x` prints `200` where C has `0x200`, `%#o` no
leading 0, and `%#g` drops the trailing zeros C keeps.

**Exit**: `#` takes C's alternate form for every conversion that has one, and
a corpus case prints each.
