---
status: open
kind: tooling
opened: 2026-10-08
---

# No harness path boots `console/`, so its `shutdown` rests on a row of its config that nothing reads

`console/system.toml` starts `acpiserver` because its shell's `shutdown` is
the only way to turn that machine off, and the kernel refuses a power-off
until the `acpi` claim's holder has handed it `\_S5`'s sleep type. No guest
test and no metal row boots that config: deleting `acpiserver` from its
`[boot] start` reds nothing, and the image then refuses every `shutdown`.

What holds today is a boot made by hand, once, at `52ef7a10f`: `cargo run --
--console-boot --build-only` exit 0 with `bin/acpiserver` on ROOT, and `cargo
run -- --console-boot` whose console carried `acpiserver: \_S5 handed to the
kernel: SLP_TYPa=0` and `power: S5 is PM1a 0x604 with SLP_TYPa=0, as the acpi
claim's holder supplied it`, with no `panicked` line. Its `shutdown` was not
typed: the shell is on the framebuffer. The rest of the path, from a `shutdown` asked to
QEMU's `guest-shutdown`, is `machine_shutdown`'s on `tests/testcases`.

## Owner

The orchestrator.

## Exit condition

A harness boot of `console/` that runs its `shutdown` and reads QEMU's
`guest-shutdown`.
