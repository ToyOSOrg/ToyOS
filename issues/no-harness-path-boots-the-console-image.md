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

What holds today is the image's build (`cargo run -- --console-boot
--build-only` exit 0 with `bin/acpiserver` on ROOT) and three boots made by
hand on 2026-10-08, in the build requests at `22f7125ce`, `52ef7a10f` and
`10a85ae49`: `cargo run -- --console-boot`, QEMU ended once the hand-over was
said. Each console carried `acpiserver: \_S5 handed to the kernel:
SLP_TYPa=0` and `power: S5 is PM1a 0x604 with SLP_TYPa=0, as the acpi claim's
holder supplied it`; the logs of the last two are kept, and the first is the
orchestrator's count from its own run log. Each opened QEMU's window on the
development machine, which the owner forbids: no such boot is made again
until a headless harness path exists, so until then nothing but the build
holds this image. Its `shutdown` was never typed: the shell is on the
framebuffer. The rest of the path, from a `shutdown` asked to QEMU's
`guest-shutdown`, is `machine_shutdown`'s on `tests/testcases`.

## Owner

The orchestrator.

## Exit condition

A harness boot of `console/` that runs its `shutdown` and reads QEMU's
`guest-shutdown`.
