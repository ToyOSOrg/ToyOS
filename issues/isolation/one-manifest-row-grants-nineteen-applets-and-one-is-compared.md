---
status: open
kind: track
opened: 2026-09-01
---

# One manifest row grants nineteen applets, and exactly one of them is compared against it

`system.toml:99` is `[programs.toybox]`, and 19 rows below it link an applet name
to `/bin/toybox`. Init resolves a symlink to its binary row
(`userland/init/src/main.rs:422-441`), so every applet runs with the union of
authority that one row grants —
`issues/isolation/toybox-is-one-row-for-nineteen-applets.md`.

**One applet is already checked, and the shape it uses is the right one.**
`tests/toyos-rust-tests/src/bin/endowment_denied.rs:39-43` runs `/bin/ps` twice,
endowed a duplicate with `Rights::ROSTER` and a duplicate without it, and its own
comment says what that buys: "the manifest's name, the kernel's demand and the
program are one line rather than three that agree by luck". That is one allowed
and one forbidden operation for one authority class on one of the nineteen. The
effective-authority side is a shipped syscall too — `SYS_ENDOWMENTS`
(`toyos-abi/src/syscall.rs:173`, wrapper at `:849`) lets a process read its own
endowment table back.

**What is missing is everything around it.** That comparison is hand-written for
one class on one applet; nothing enumerates the nineteen links; no other
authority class is covered; and the question the union raises — what can `echo`
reach that `echo` has no business reaching, because `hexdump` needed it — is
unanswered for eighteen of them.

**What to build.** Enumerate the installed links, resolve each through the real
init lookup, and compare effective authority against an explicit per-applet
policy table. Then exercise one allowed and one forbidden operation per distinct
authority *class* — classes, not applets, because nineteen applets do not have
nineteen distinct authorities, and `endowment_denied` already covers one of them.

**The circularity to avoid, and it is the strongest reason to build this.** A
checker that reimplements init's resolver agrees with init by construction. Parse
the manifest and the symlink inventory independently and compare *their* outputs
against the resolver's; the differential is the oracle. Nothing in `src/build.rs`
does this today — its manifest gates check the manifest's internal consistency
and say nothing about per-applet effective authority.

**Reuse.** Every multicall binary and every manifest audit wants it; toybox is
merely the first place one row grants many programs.
