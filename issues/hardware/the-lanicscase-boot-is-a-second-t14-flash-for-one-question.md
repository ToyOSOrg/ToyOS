---
status: open
kind: tooling
opened: 2026-09-13
---

# `tests/lanicscase` is a second T14 flash, and it earns it only while the card is silent

`tests/lanicscase` is `tests/lancase` with one `args` row — netd's
`--provoke-message`, which writes one enabled cause to `ICS` so the kernel's
`pcidev: slot N took its first message` record says whether delivery works at
all. It is a second arm of `lan_dhcp_lease`, a second image flashed to the
stick, a second boot of the machine and four rows of `tests/metal-profile.toml`
(`boot.lanicscase.{complete_ms,back_secs,stick_secs}`, `list.lanicscase.job_ms`).

**A count of no messages is two facts** — a part nothing made speak and a
message that reached no CPU — and only this arm separates them. On a card that
raises its own `LSC` it records exactly what `tests/lancase` records, so from
that moment it is a boot of the machine that answers a question already
answered.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

The shipping `lancase` arm recording `pcidev: slot N took its first message`
without the actuator. Then the actuator has no question left and the arm is
four files: `tests/lanicscase/system.toml`, its row in `src/build.rs`'s
`ALL_CONFIGS`, the second `metal::Arm` in `tests/toyos.rs`'s `LANCASE` with
`lan::provoked_on_metal`, and the four `tests/metal-profile.toml` rows.

Until then the machine has read neither fact: nothing on this branch has run on
the T14.
