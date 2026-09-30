---
status: open
kind: tooling
opened: 2026-09-16
---

# netd's lease probe answers a question its own lines already answer

netd's `--exit-with-lease` brings the card up and serves exactly as the
shipping boot does for a bounded window, leaves `/log/lease.txt` on the log
volume one durable line at a time (the bring-up's own words, every link change,
the lease, what the driver and the MAC counted each way), and ends with
`toyos_i219::lease::Verdict`'s code. It existed because every line netd printed
about the same things was a console write, and on the T14 a console write
reached no file. Every program's lines reach `/log` through its log ring now,
and the talking boot's `lan_dhcp_lease` judge reads the lease off netd's own
lines, so the probe's T14 boot, `tests/lanleasecase`, is gone. What still arms
it is `tests/e1000leasecase` and the `lan_lease_report` QEMU registration,
whose link-flap check reads the report.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

The flap judged off netd's own lines. Then netd's `--exit-with-lease`,
`userland/netd/src/report.rs`, `toyos-i219/src/lease.rs`'s report lines,
`tests/e1000leasecase` and the `lan_lease_report` QEMU registration go.

An earlier form of this arm, `--exit-with-phy-outcome` on `d409139f^:tests/lanphycase`,
ended right after the bring-up with the PHY's outcome; its codes are still
`Verdict::NotLeased`'s, so a code read off one of its boots means what
`toyos_i219::phy::Outcome` says.
