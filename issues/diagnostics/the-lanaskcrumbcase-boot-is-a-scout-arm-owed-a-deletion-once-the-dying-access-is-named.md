---
status: open
kind: tooling
opened: 2026-09-23
---

# `tests/lanaskcrumbcase` is a scout arm, and it is owed a deletion once the access the T14 dies on is named

`tests/lanaskcrumbcase` is `tests/lanaskcase` with one `args` row — netd's
`--exit-with-ask-crumbs`, under which netd puts exactly `--exit-with-mdio-ask`'s
question to §4.5.2's arbitration and appends one line to `/log/crumbs.txt` and
`sync_all`s it immediately before and immediately after every register access
from `EXTCNF_CTRL` on, and around the dwell it holds the claim for. It exists
because the ask boot left a machine the owner had to power off by hand and a
stick whose log ends in SMP bring-up, so nothing said which access the machine
was in; a line already on the device before the access and another after it
says it.

It costs a fifth image, the `lan_ask_crumb_trail` registration and metal row,
six rows of `tests/metal-profile.toml`, and `toyos_i219::crumbs`'s `Deed`,
`Step::Before`/`Step::After`, `Witnessed`, `around` and `witnessed` — the whole
of which exists for this one reading. The trail also stretches every wait the
question takes on a clock: the bound is unchanged, so fewer samples fit inside
it, and that is a difference from the ask boot this arm's reading has to carry.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

One boot of the arm on the T14 read — the last durable line of `crumbs.txt`
naming the access, or a whole trail saying the death is later than netd. Then
the arm goes, all of it: netd's `--exit-with-ask-crumbs`, `i219::ask_with_crumbs`
and the fifth entry of `ACTUATORS`, `toyos-i219`'s `ask::after_reset_witnessed`
and the `crumbs` items above with their tests, `tests/lanaskcrumbcase`,
`tests/e1000askcrumbcase`, their two rows in `src/build.rs`'s `ALL_CONFIGS` and
the fifth entry of its `INTEL_ACTUATORS`, `tests/toyos.rs`'s
`lan_ask_crumb_trail` registration and metal row with `LANASKCRUMBCASE`,
`lan::ask_trailed_on_metal`, `lan::whole_ask_trail`, `lan::ask_trail`,
`lan::lan_ask_crumb_trail`, and the six `tests/metal-profile.toml` rows.
