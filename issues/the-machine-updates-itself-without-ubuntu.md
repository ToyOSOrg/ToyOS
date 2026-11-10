---
status: open
kind: track
opened: 2026-09-24
---

# The machine updates itself, without Ubuntu, and boots only what the owner signed

The owner's end state (ruling 2026-09-26): **no Ubuntu at all**. The T14's
stick carries ToyOS and ToyOS is installed on its NVMe; the T14 is a
playground, so a broken driver may wipe that NVMe and the next install writes it
again. Updates are `ssh <machine> update < image` — a normal `exec`, the image on
stdin — and the machine installs nothing the owner did not sign.

## Stage 1 — slots, signatures and `update`, proven in QEMU (done)

Done: the signed image (`toyos-update`), the two slots and their table, and
the loader's verification.

- **`update`** holds the slot table and the idle slot's two partitions, which
  init claims against the ROOT the kernel holds, writes the image and moves the
  mark last, after an fsync of its own writes. **`swap`** replaced sshd's
  subsystem the same way.
- **Keys**: a throwaway per checkout, kept in its `target/`, for QEMU, CI and
  the metal loop, whose loaders keep a floor per image; the owner's
  (`--owner-key`, `--update-image`, minted by `--signing-key-new`) for an image
  installed on the owner's machine, whose loader keeps the machine's floor.

## Stage 2 — fix forward: the loader boots the marked slot or nothing (done)

Owner ruling, 2026-11-05, answering #1191's open decisions: updates fix
forward, with no automatic fallback to the previous slot.

Done: a pass verifies the marked slot alone. Where it is refused, the loader
names the refusal on the console and in `loader.log`, reads nothing of the
other slot and ends as its panic does: the machine powers off, or resets
where a `BootNext` bought this boot and the default entry is another. The
kernel is told only the slot it was booted from. #1196 took the death-driven
fallback out with the black-box page.

`a_bent_supervisor_is_refused_and_nothing_boots` holds it on both
architectures' QEMU machines. It has no metal row: the metal loop judges only
a boot that stopped cleanly, so a refused slot on the T14 is stage 6's exit in
`issues/the-loader-does-only-what-must-precede-the-handover.md`. A one-shot
`update --once` is a request, not a fallback, and stays that track's stage 5.

## Stage 3 — the T14 installs ToyOS on its NVMe

The machine boots the stick and installs onto its own NVMe, then takes
`ssh t14 update < image` for every change after. It waits on stage 5 of
`issues/the-loader-does-only-what-must-precede-the-handover.md`,
whose `update --boot-first` puts the NVMe loader's entry first; taking Ubuntu
out of `toyos-metal`'s loop is that track's too.

**Exit**: with the stick pulled, the T14 boots ToyOS off its NVMe, and a
kernel change sent with `ssh t14 update < image` boots at the next reset.

## Later stages — the end state, which no earlier stage may block

Each a stage: an image-based read-only system, a verified boot chain,
anti-rollback against the machine in hand
(`issues/the-anti-rollback-floor-is-a-firmware-variable.md`), pull-based
updates from a release server over HTTPS with The Update Framework's roles and
metadata, and content-addressed chunked delta updates. Until the verified
boot chain, the loader is the one binary no signature covers, and a writable
ESP is the gap.
