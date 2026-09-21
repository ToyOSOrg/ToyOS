---
status: open
kind: tooling
opened: 2026-09-21
---

# `tests/lanaskcase` is a scout arm, and it is owed a deletion once its one answer is in

`tests/lanaskcase` is `tests/lancase` with one `args` row — netd's
`--exit-with-mdio-ask`, under which netd resets the I219, registers §4.5.2's
software request in `EXTCNF_CTRL` whoever else's bit stands, samples the
register until the bit reads back set or `toyos_i219::ask::BOUND_NANOS` has
passed, withdraws the request, and ends with `toyos_i219::ask::Reading`'s code.
The card is never brought up on that boot. It exists because two boots of
`tests/lanphycase` on the T14 ended with the MDIO interface named as another
agent's for the whole of the bring-up's wait, and a bring-up that waits for the
interface to go free never learns what asking would have been answered.

It is a fourth arm of `lan_dhcp_lease`, a fourth image, six rows of
`tests/metal-profile.toml`, and its codes fill what `phy::Outcome` left of the
block under 128: `ask::Reading` ends at 127 and no third table fits beside the
two.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

One boot of the arm on the T14 read, and the bring-up in
`toyos-i219/src/phy.rs` changed — or deliberately left — on that reading. Then
the arm goes, all of it: `toyos-i219/src/ask.rs` and its tests,
`tests/lanaskcase`, `tests/e1000askcase`, their two rows in `src/build.rs`'s
`ALL_CONFIGS` and the third entry of its `INTEL_ACTUATORS`, the fourth
`metal::Arm` in `tests/toyos.rs`'s `LANCASE` and the readback `lan::on_metal`
decodes for it, `lan_mdio_ask_exit_code` and its `tests/test-durations` row, the
six `tests/metal-profile.toml` rows, and netd's `--exit-with-mdio-ask`.
