---
status: open
kind: tooling
opened: 2026-10-03
---

# The update rig the guest cut deleted is still cited

#660 (`06788146b`) deleted `tests/common/update.rs` with its `Rig`,
`tests/common/fwvars.rs` and `tests/updatecase/system.toml`. `git grep -n
'fwvars\|updatecase\|common/update\.rs\|Rig::\|vars::plant\|vars::live'` finds
nine lines in three files, each planning or describing a test on them:

- `issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`,
  six lines. Stage 3's exit takes `fwvars::live` as
  `update_floor_is_the_images_own`'s oracle. Stage 5 says
  `tests/updatecase/system.toml` grants `slots` and that two tests run
  `updatecase`; one of its negative controls signs with `Rig::update` and
  reads the floor with `fwvars::live`, which its oracles name too.
- `issues/boot-media/the-loader-never-sets-the-firmwares-memory-overwrite-request.md`,
  two lines: its exit's `mor_is_set_where_defined` plants and reads the vars
  store with `vars::plant` and `vars::live`.
- `issues/build/the-kernel-console-split-does-not-re-arm-across-a-guest-reset.md`,
  one line: the reset in place it describes is `Rig::boot`'s.

What each should name is the tier its test has now, and stage A of
`issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` gives
it: `update_floor_is_the_images_own`, `update_refusals_boot_the_other_slot`
and `update_grant_refuses_a_stray_partition` are host tests there and guest
tests in the loader track, and the tree holds no `update_*` test today.

**Owner**: that stage A, whose pull request lands those tests and with them
the oracle and the rig a sentence can name.

**Exit**: the `git grep` above finds this file alone: each sentence names the
tier that holds its test and an oracle, a rig or a config the tree has, or has
gone with the test it described.
