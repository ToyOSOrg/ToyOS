---
status: open
kind: defect
opened: 2026-09-03
---

# Four `KNOWN` masks are hand-copied unions of `toyos-abi`'s constants, and a bit added there silently falls out of them

`SYS_MMAP`, `SYS_OPEN`, `SYS_PROCESS_WAIT` and the inbox watch each refuse a
flag word carrying a bit outside a `KNOWN` mask. Every mask is written on the
kernel side as a union of `toyos-abi`'s own constants —
`MmapProt::READ.0 | MmapProt::WRITE.0` and its three siblings — because the
change that added them was not allowed to touch the shared sysroot.

**Measured, in the adversarial review of #377**: adding
`pub const EXEC: Self = Self(4)` to `MmapProt` leaves `toyos-abi` and the kernel
compiling clean, and `MMAP_PROT_KNOWN` — still `READ.0 | WRITE.0` — then refuses
a bit the ABI defines. Fail-closed, so it is a maintenance hazard and not a
hole: the caller is told `InvalidArgument` rather than served something else.
The same union is copied a third time into each guest arm's local `const KNOWN`,
so a drift would take the test with it and the arm would still pass.

**The fix is one line per word and the precedent is in the same file.**
`NAMESPACE_FLAGS_KNOWN` (`toyos-abi/src/syscall.rs:1340`) is the mask that
cannot drift, because it sits beside the constants it unions and every reader
names it. Four more of those — for `MmapProt`, `MmapFlags`, `OpenFlags` and
`WNOHANG` — delete this entry, the three copies in the kernel and the three in
the tests. It is an ABI change, so it lands on its own single-commit branch by
the abi-split rule.
