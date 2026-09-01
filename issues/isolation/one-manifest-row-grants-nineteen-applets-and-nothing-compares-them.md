---
status: open
kind: track
opened: 2026-09-01
---

# One manifest row grants nineteen applets, and nothing compares intended authority with effective authority

`system.toml:99` is `[programs.toybox]`, and 19 rows below it link an applet name
to `/bin/toybox`. Init resolves a symlink to its binary row, so every applet runs
with the union of authority that one row grants —
`issues/isolation/toybox-is-one-row-for-nineteen-applets.md`.

**What the missing observation is, precisely.** Not nineteen behavioural output
contracts — `cat` behaving like `cat` is not the question. The question is
*per-applet authority*: what can `echo` reach that `echo` has no business
reaching, because `hexdump` needed it.

**What to build.** Enumerate the installed links, resolve each through the real
init lookup, and compare the effective authority against an explicit per-applet
policy table. Then exercise one allowed and one forbidden operation per distinct
authority class — classes, not applets, because nineteen applets do not have
nineteen distinct authorities.

**The circularity to avoid.** A checker that reimplements init's resolver agrees
with init by construction. Parse the manifest and the symlink inventory
independently and compare *their* outputs against the resolver's; the differential
is the oracle.

**Reuse.** Every multicall binary and every manifest audit wants this; toybox is
merely the first place one row grants many programs.
