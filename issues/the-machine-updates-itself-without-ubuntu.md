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
the loader's verification. The loader boots the marked slot and today the
other where the marked one is refused; the kernel logs which and why. That
fallback leaves in stage 2.

- **`update`** holds the slot table and the idle slot's two partitions, which
  init claims against the ROOT the kernel holds, writes the image and moves the
  mark last, after an fsync of its own writes. **`swap`** replaced sshd's
  subsystem the same way.
- **Keys**: a throwaway per checkout, kept in its `target/`, for QEMU, CI and
  the metal loop, whose loaders keep a floor per image; the owner's
  (`--owner-key`, `--update-image`, minted by `--signing-key-new`) for an image
  installed on the owner's machine, whose loader keeps the machine's floor.

## Stage 2 — fix forward: the loader boots the marked slot or nothing

Owner ruling, 2026-11-05, answering #1191's open decisions: updates fix
forward, with no automatic fallback to the previous slot.

The loader falls back today in three ways, all of which go:

- `policy::order` names the other slot after the marked one, and
  `slot::choose` boots it when the marked one is refused;
- `Refusal::Died` refuses an image whose last boot the record says died or
  hung, so the other slot boots instead;
- where no slot verifies but a dead one, `slot::choose` boots that one,
  which may be the other slot.

A pass verifies the marked slot alone and boots it whether or not its last
boot died. Where it is refused, the loader names the refusal on the panel and
in `loader.log` and powers the machine off. `slot-refused=` and the kernel's
line that logs it go with the fallback. A one-shot `update --once` is a
request, not a fallback, and stays the loader track's stage 5.

With the fallback gone, marking an image dead in the boot record on the
black-box page no longer refuses anything, but the next boot still reports the
last boot's death; #1196 (the memory-overwrite request) decides what carries
that report if the page goes. This stage removes only the fallback's use of
the record.

**Exit**: `policy`'s host tests hold that a pass names only the marked slot
and that a dead image in it is still booted.
`a_bent_supervisor_is_refused_and_the_other_slot_boots` and its metal row
`bent_supervisor_falls_back` become a bent marked supervisor refused by name,
nothing booted and the machine powered off; putting the other slot back into
`policy::order` reds them.

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
