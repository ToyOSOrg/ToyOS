---
status: open
kind: track
opened: 2026-09-01
---

# One manifest row grants every applet, and exactly one of them is compared against it

`system.toml:112` is `[programs.toybox]`, and every `[symlinks]` row below it links an applet name
to `/system/bin/toybox`. Init resolves a symlink to its binary row
(`userland/init/src/main.rs:422-441`), so every applet runs with the union of
authority that one row grants —
`issues/isolation/toybox-is-one-row-for-every-applet.md`.

**One applet is already checked, and the shape it uses is the right one.**
`tests/toyos-rust-tests/src/bin/endowment_denied.rs:39-43` runs `/system/bin/ps` twice,
endowed a duplicate with `Rights::ROSTER` and a duplicate without it, and its own
comment says what that buys: "the manifest's name, the kernel's demand and the
program are one line rather than three that agree by luck". That is one allowed
and one forbidden operation for one authority class on one of them. The
effective-authority side is a shipped syscall too — `SYS_ENDOWMENTS`
(`toyos-abi/src/syscall.rs:173`, wrapper at `:849`) lets a process read its own
endowment table back.

**The enumeration exists now, and it reds on the image that boots.**
`endowment_denied`'s `every_applet_holds_only_what_its_policy_names` reads the
links off `/system/bin` with `read_dir`/`read_link`, parses `/system/etc/system.manifest` with
a parser that is neither init's nor `toyos_manifest`'s, and holds each applet
against `APPLET_NEEDS`. With its declared list empty, the guest answers:

```
assertion `left == right` failed: the authority /system/bin/toybox's one row hands
applets that have no use for it is not the list this test declares
  left: ["cat: receive soundd", "cp: receive soundd", "echo: receive soundd",
         "free: receive soundd", "grep: receive soundd", "hexdump: receive soundd",
         "ls: receive soundd", "mkdir: receive soundd", "mv: receive soundd",
         "ps: receive soundd", "pwd: receive soundd", "reboot: receive soundd",
         "rm: receive soundd",
         "shutdown: receive soundd"]
 right: []
```

Every applet but `tone` in the test image holds a connector to soundd
because `tone` needs one. That list is now `DECLARED_OVER_GRANTS`, so the size
of the defect cannot grow without a red naming what grew.

**What is still missing is the shipped image.** A guest test boots
`tests/testcases/system.toml`, whose `toybox` row is `receives = ["soundd"]` and
carries no `syscap` at all. The row this record opened on — `system.toml:112`,
`syscap = ["power", "roster"]` across every link — is measured by nothing:
the differential that would see `/system/bin/echo` holding `Rights::POWER` has to read
`system.toml` and ROOT's link list on the host, in `cargo test --lib`,
because no boot carries that manifest.

**The differential reads one row for every link, and the proposed fix is exactly
what that would ignore.** `declared` tries `row(invoked path)` *before*
`read_link` (`userland/init/src/main.rs:433-441`), so a row keyed `shutdown` with
`path = "/system/bin/shutdown"` would win — while this checker resolves every link to
`/system/bin/toybox`, reads that one row, and would keep reporting the union. Whoever
builds the per-path row extends the checker to ask `row(invoked path)` first, in
the same landing: a red after that fix is this checker's and not the fix's.

**The circularity that was avoided, and it is the strongest reason this shape is
the shape.** A checker that reimplements init's resolver agrees with init by
construction. The two sides above come from different places — the links off the
image, the rows off the rendered manifest, the policy from a table nothing in
the build wrote. Nothing in `src/build.rs` does this either: its manifest gates
check the manifest's internal consistency and say nothing about per-applet
effective authority.

**Reuse.** Every multicall binary and every manifest audit wants it; toybox is
merely the first place one row grants many programs.
