---
status: owner
kind: question
opened: 2026-09-03
---

# Eight storage decisions are the owner's, each with the orchestrator's recommendation

`issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md` records
what was ruled on 2026-09-03. These eight were not; a one-line answer to each
closes this file and moves into that one.

1. **How `/system` is versioned.** One ROOT partition per release with the
   bootloader choosing — recommended, because the kernel's read path then
   needs no snapshot logic — or one subvolume per release inside a single
   ROOT.
2. **The hierarchy's names.** `/system`, `/apps`, `/home`, `/media`, keeping
   `/boot`, `/log`, `/tmp`. Recommended as written.
3. **`/log` as its own FAT32 partition.** Keep through the dev phase so a Mac
   can read the stick — recommended — or fold it into DATA now.
4. **Where the hierarchy lands.** Inside root filesystem PR 2, which lays out
   the root partition — recommended — or as its own pull request after it.
5. **Users.** File the track now as staged work — recommended — or wait until
   the hierarchy exists.
6. **NTFS write policy.** Read-only, refusing write on a dirty or hibernated
   volume by name — recommended — or read-write from the start.
7. **Installation's shape.** GPT plus the designation stamp, the installer a
   later program — recommended.
8. **Verified boot.** Record it as a later stage of `/system`'s immutability —
   recommended — or not at all yet.
