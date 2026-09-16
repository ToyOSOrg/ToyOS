---
status: open
kind: tooling
opened: 2026-09-16
---

# `tests/lanphycase` is a third T14 flash, and it earns it only while netd's console line cannot cross

`tests/lanphycase` is `tests/lancase` with one `args` row — netd's
`--exit-with-phy-outcome`, under which netd ends right after the bring-up with
`toyos_i219::phy::Outcome`'s code for what the PHY answered. It exists because
the line netd prints about the same outcome is a console write, and on the T14
a console write reaches no file: the kernel's `exit: netd pid=N code=N` record
is the one word of a process that crosses
(`issues/diagnostics/the-cable-judge-reads-three-netd-records-that-cannot-arrive-on-the-t14.md`).
It is a third arm of `lan_dhcp_lease`, a third image flashed to the stick, a
third boot of the machine and six rows of `tests/metal-profile.toml`.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

Userland's console reaching the stick
(`issues/diagnostics/the-log-staged-three-things-it-never-built.md` §1), after
which the shipping `lancase` arm carries netd's own line naming the outcome
and the probe answers a question already answered. Then the arm is five
files: `tests/lanphycase/system.toml`, its row in `src/build.rs`'s
`ALL_CONFIGS`, the third `metal::Arm` in `tests/toyos.rs`'s `LANCASE` and the
readback `lan::on_metal` decodes for it, the six `tests/metal-profile.toml`
rows — and netd's `--exit-with-phy-outcome` with `tests/e1000phycase` and
`lan_phy_exit_code`, the QEMU arm that proves the channel.

Until then the machine has read nothing through it: no boot of this arm has
run on the T14.
