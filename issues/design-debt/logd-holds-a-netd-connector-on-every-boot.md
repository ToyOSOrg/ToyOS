---
status: open
kind: defect
opened: 2026-09-08
---

# `logd` holds a `netd` connector on every boot, and streams on almost none

`system.toml`'s `[programs.logd]` carries `receives = ["netd"]` on every image
this tree builds. The record stream it is for is armed by a boot parameter
(`logstream=`), which no shipping boot carries, so the one process that reads
every record every CPU wrote holds an outbound network connector for the whole
life of a machine that will never open a connection.

`init` builds a program's endowment from its manifest row before it spawns it
and has no way to make a row conditional on the parameter line, so the authority
is static while the feature is not. `sshd` is kept out of `[boot] start`
entirely for a weaker version of the same argument.

The exit condition is one of: `init` learns to grant a connector only when the
boot asked for what it is for; or the manifest gains a way to say "this row is
armed by this parameter"; or the tree decides an unopened connector is not
authority worth withholding and this file is closed by that ruling.
