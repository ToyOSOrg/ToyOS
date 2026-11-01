---
status: open
kind: track
opened: 2026-10-11
---

# General-purpose crates are published, and ToyOS consumes them

**The owner's direction:**
- Crates that are general-purpose get published on crates.io with an owner of their own (an AI owner).
- ToyOS becomes one consumer of each. It may request changes, but the crate stays general.
- Not during the current rapid-development era: nothing is built for this until the owner says go.

**Candidates** (scan of the restructured tree, 2026-10-11), best first:
1. `toyos-fat32-check`: FAT32 checker written from Microsoft's fatgen103. No ToyOS dependencies, `no_std`.
2. `toyos-fat32`: FAT32 read/write hardened against hostile volumes. It needs `toyos-wallclock` folded in, and its tests made self-contained.
3. The bcachefs engine (`lib/bcachefs/src/engine`), split out of `lib/bcachefs` once the interim format leaves it.
4. `toyos-microcode` and `toyos-usbhid`.
5. Later: the TCP stack (once `wire`'s grab-bag is factored out), the SSH server (once its track and defect issues close), and the AML interpreter (once it is decoupled from `toyos-acpi`).

**Not candidates:** sha2/sha2-hw, random, crc, wallclock and tmpdir. Widely used crates already do their jobs.

**What exists:** `build/sdkversion.rs`'s `PUBLISHED` already publishes six SDK crates from the monorepo, with crates.io trusted publishing.

**Open design:**
- Publish from the tree, or move out to a repository of its own? Publishing from the tree keeps atomic changes and self-hosting. A repository of its own gives the crate a real owner and its own history.
- How an owning agent is run.
- How ToyOS pins and requests.

**Exit:** a crate ToyOS used to build from its tree is published under its own owner, and ToyOS builds against the published version.

**Owner:** the owner (Jan) decides when it starts.
