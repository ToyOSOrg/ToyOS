---
status: open
kind: tooling
opened: 2026-09-16
---

# `tests/lanleasecase` is a third T14 flash, and it earns it only while netd's console line cannot cross

`tests/lanleasecase` is `tests/lancase` with one `args` row — netd's
`--exit-with-lease`, under which netd brings the card up and serves exactly as
the shipping boot does for a bounded window, leaves `/log/lease.txt` on the log
volume one durable line at a time (the bring-up's own words, every link change,
the lease, what the driver and the MAC counted each way), and ends with
`toyos_i219::lease::Verdict`'s code. It exists because every line netd prints
about the same things is a console write, and on the T14 a console write
reaches no file
(`issues/diagnostics/the-cable-judge-reads-three-netd-records-that-cannot-arrive-on-the-t14.md`):
the kernel's `exit: netd pid=N code=N` record and a file netd writes itself are
the two words of a process that cross. It is the `lan_lease_report` metal row,
a third image flashed to the stick, a third boot of the machine and six rows of
`tests/metal-profile.toml`.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

Userland's lines reaching the stick, which they do through each program's log
ring (`issues/kernel/logging-records-from-every-producer-and-a-kernel-that-waits-on-nobody.md`), after
which the shipping `lancase` arm carries netd's own lines about the lease and
the probe answers a question already answered. Then the arm is:
`tests/lanleasecase/system.toml`, its row in `src/build.rs`'s `ALL_CONFIGS`,
the `lan_lease_report` metal row in `tests/toyos.rs` with `LANLEASECASE` and
`lan::leased_on_metal`, the six `tests/metal-profile.toml` rows,
`userland/netd/src/report.rs` and `toyos-i219/src/lease.rs`'s report lines —
and netd's `--exit-with-lease` with `tests/e1000leasecase` and the
`lan_lease_report` QEMU registration, the arm that proves the channel.

An earlier form of this arm, `--exit-with-phy-outcome` on `tests/lanphycase`,
ended right after the bring-up with the PHY's outcome; its codes are still
`Verdict::NotLeased`'s, so a code read off one of its boots means what
`toyos_i219::phy::Outcome` says.
