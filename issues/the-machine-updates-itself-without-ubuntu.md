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

## Finished-distro update policy

Owner decision, 2026-10-18: personally modified ToyOS system code is maintained
as the user's own build, with upstream merges and rebuilds through ordinary
development tools. The updater need not carry local source modifications across
distro updates. This does not change preservation of settings, data or installed
apps. Custom-build update-source selection and return to distro builds remain
to design under the existing image-verification and trust authority rules.

Owner decision, 2026-10-18: maintain one current stable release. Ongoing
security fixes target it; users must upgrade to remain supported.
Release cadence and transition communication remain product requirements to
refine; this does not change postponement or the no-forced-restart policy.

Owner decision, 2026-10-20: fix forward. No OS downgrades or rollbacks in the
current release scope, including automatic fallback to an older OS after an
activated update fails to boot. Ship a newer fixed version. This supersedes
the older-image recovery direction and the fallback target in stage 1 below.
That stage records prior implementation evidence, not the newly approved end
state. Reconcile loader selection, activation and version-floor rules; do not
claim the current fallback behavior satisfies this decision. Safe staging
remains required. A failed download or incomplete staging must not damage the
current installation. Owner confirmation, 2026-10-20: a boot-medium workflow
that applies a newer fixed release while preserving data remains deferred.
Manual rescue or fresh installation is acceptable initially after an unbootable
update. This does not authorize erasing data or claim fresh installation
preserves it. Implementation is not commissioned by this record.

Owner decision, 2026-10-16: prepare verified OS updates automatically and
activate them at the next normal reboot, with an option to postpone. Never
force a restart. This is the product target, not a change to the current
development deployment loop or a claim that automatic fetching exists.

Reuse the signed-image and inactive-slot installation path. Fetching,
scheduling and the user's choice belong in userland. The current `update`
command installs and selects the new slot in one operation; preparation and
next-boot selection must support postponement coherently, including a reboot
or interrupted download. A downloaded image is not an activated update.
Application updates remain a separate policy.

## Authority to change OS trust

Owner decision, 2026-10-20: first-release installation may require documented
one-time firmware trust setup. Verified boot must be active afterward;
unchanged factory trust settings are not a release gate. Validate enrollment,
key ownership and recovery on both benches. This installation allowance does
not change the separate authorization policy below.

Owner decision, 2026-10-16: an explicitly authorized administrator may change
which OS signing keys the machine trusts remotely. Local presence is not a
ToyOS policy requirement for that operation. This authority is distinct from
permission to install an ordinary update signed by an already-trusted key;
remote login alone grants neither.

Enrollment, key replacement, firmware support and coordination with encrypted
storage's key-release policy remain design work. Changing OS trust does not
itself grant recovery of a user's locked storage. A replacement OS can observe
data when subsequently unlocked; do not promise isolation from an OS the
machine has been authorized to run.

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
- **Keys**: a throwaway per checkout, kept in its `target/`, for QEMU, CI and
  the metal loop, whose loaders keep a floor per image; the owner's
  (`--owner-key`, `--update-image`, minted by `--signing-key-new`) for an image
  installed on the owner's machine, whose loader keeps the machine's floor.

## Stage 2 — the T14 installs ToyOS on its NVMe

Owner decision, revised 2026-10-18: the first-release installation medium boots
into ToyOS's desktop. A desktop entry opens a graphical installer, replacing
the earlier console/CLI requirement. It offers independent language, regional
format, keyboard and time-zone selection, also available later in settings.
Installation and first-account
setup must work offline from a small base image; optional applications are
downloaded separately.

Owner decision, 2026-10-18: the boot-medium workflow for reinstalling a broken
system while preserving existing user data is deferred beyond the first
release. Safe installation and the update-failure contract remain required.

The machine boots the stick and installs onto its own NVMe, then takes
`ssh t14 update < image` for every change after. It waits on stage 5 of
`issues/the-loader-does-only-what-must-precede-the-handover.md`,
whose `update --boot-first` puts the NVMe loader's entry first; taking Ubuntu
out of `toyos-metal`'s loop is that track's too.

**Exit**: with the stick pulled, the T14 boots ToyOS off its NVMe, and a
kernel change sent with `ssh t14 update < image` boots at the next reset.

## Later stages — the end state, which no earlier stage may block

- **An image-based, read-only system**: `/system` is the signed ROOT and
  nothing else, and nothing the machine runs is outside an image or a package.
- **A verified boot chain**: UEFI Secure Boot with the owner's key over the
  loader. Until then the loader is the one binary no signature covers, and a
  writable ESP is the gap.
- **Anti-rollback that holds against the machine in hand**:
  `issues/the-anti-rollback-floor-is-a-firmware-variable.md`.
- **Pull-based updates** from a release server over HTTPS, with The Update
  Framework's roles and metadata over the same signed header — `update` takes
  the bytes on stdin and does not care how they came.
- **Content-addressed, chunked delta updates**: the header already names each
  section by hash, so a chunked form is the same signature over the same
  hashes, and only chunks the idle slot lacks cross the wire.
- **User data never touched by an update**: `/home` and `/apps` are on DATA,
  which no slot includes.
- **Apps updated separately** through `pkg`.
