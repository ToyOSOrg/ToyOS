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

- **A signed image** (`toyos-update`): a header naming a monotonic version and
  the SHA-256 of the kernel, its boot parameter and ROOT, signed with Ed25519
  over OpenSSH's SSHSIG message in the `toyos-image` namespace — so
  `ssh-keygen -Y sign` signs one and `ssh-keygen -Y verify` checks ours. The
  signature covers hashes, never bytes.
- **Two slots**, each a FAT volume (kernel, parameter, signed header) and a
  ROOT, named by a slot table (two copies, a sequence, the writer writes the
  copy that is not current). Every image carries slot A; an image built for a
  machine carries an empty slot B.
- **The loader** boots the marked slot and falls back to the other where the
  marked one is refused — unsigned, another key's, a flipped byte, a version
  under the floor — or died on its last boot; the kernel logs which and why, and
  `logd` carries it to `/log`.
- **`update`** holds the slot table and the idle slot's two partitions, which
  init claims against the ROOT the kernel holds, writes the image and moves the
  mark last, after an fsync of its own writes. **`swap`** replaced sshd's
  subsystem the same way.
- **Keys**: a throwaway per build process for everything that stays on the Mac;
  the owner's (`--owner-key`, `--update-image`, minted by `--signing-key-new`)
  for what leaves it.

## Stage 2 — the T14 installs ToyOS on its NVMe and updates without Ubuntu

What `toyos-metal` still runs Ubuntu for, each of which this stage replaces:

1. **Writing the image** — `wipefs` and `dd of=/dev/sda` under a sudoers rule.
   Replaced by the machine booting the stick and installing onto its own NVMe,
   then `ssh t14 update < image` for every change after.
2. **Choosing the next boot** — `efibootmgr --create-only`, `--delete-bootnum`,
   `--bootnext`. The loader already points `BootNext` at itself; an install
   writes its own entry once.
3. **Reading a boot's verdict** — `dd if=/dev/sda3` and `mount -o ro` of the log
   partition. Replaced by `logd`'s record stream and `ssh … cat`.
4. **Reboots and liveness** — `reboot`, `true`, `date -u +%s`, the `/sys`
   identity reads of the stick, and the loop's wait for Ubuntu's sshd to come
   back after every ToyOS boot.
5. **The runner key and `ssh t14`** themselves reach Ubuntu's sshd, not ToyOS's.

**Exit**: a kernel change reaches the T14 and boots with Ubuntu never started;
a slot with a flipped byte, no signature or a lower version is refused and the
other boots; a boot that dies falls back on its own — each on the T14.

## Later stages — the end state, which no earlier stage may block

- **An image-based, read-only system**: `/system` is the signed ROOT and
  nothing else, and nothing the machine runs is outside an image or a package.
- **A/B slots with automatic rollback**: stage 1's fallback, plus a boot that
  confirms itself healthy before the floor rises (today the floor rises only on
  a boot that hands the machine back on purpose).
- **A verified boot chain**: UEFI Secure Boot with the owner's key over the
  loader. Until then the loader is the one binary no signature covers, and a
  writable ESP is the gap.
- **Anti-rollback that holds against the machine in hand**:
  `issues/boot-media/the-anti-rollback-floor-is-a-firmware-variable.md`.
- **Pull-based updates** from a release server over HTTPS, with The Update
  Framework's roles and metadata over the same signed header — `update` takes
  the bytes on stdin and does not care how they came.
- **Content-addressed, chunked delta updates**: the header already names each
  section by hash, so a chunked form is the same signature over the same
  hashes, and only chunks the idle slot lacks cross the wire.
- **User data never touched by an update**: `/home` and `/apps` are on DATA,
  which no slot includes.
- **Apps updated separately** through `pkg`.
