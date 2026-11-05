# Purpose and scope

ToyOS is a Rust-first desktop and development OS for its authors and their
friends: responsive under load, efficient with resources, secure through
explicit authority, and easy to understand and change. Technical early adopters
are the audience. There is no formal launch trial or external audit, but
correctness, security and performance still need evidence.

## Release one

- **Develop ToyOS on ToyOS**: edit, review, build, test and debug, including
  rebuilding the Rust and C/C++ toolchain. A downloaded compiler seed and a
  second physical test machine are acceptable. Native terminal tools are
  enough, and development works offline once sources and dependencies are
  prepared.
- **AI-assisted coding** uses ordinary applications and permissions; paid cloud
  APIs are acceptable. The coding agent is chosen last, and none may require a
  closed application without a maintainable native port.
- **Browsing and media** come from Chromium, without DRM. The reference is
  YouTube at a smooth 1080p60 while a full build keeps making progress.
- **Bluetooth playback**, with AirPods Pro 3 as the reference headset. Headset
  calls and vendor ecosystem features are not implied.
- **Passkeys** on websites, with USB security keys and credentials stored on the
  laptop. Transfer and cloud synchronisation can wait.
- **Native programs**: ordinary Rust programs, and C/C++ ports through libc.
  SQLite is in scope; PostgreSQL and Nginx are later targets whose needs
  constrain the architecture now. There is no compatibility layer for another
  OS's binaries.

## Performance

- Build throughput is at least Linux's on matched hardware and work.
- Applications open as fast as on Linux, launch to usable state, cold and warm,
  with at most imperceptible differences.
- Hot paths are optimised even when today's workloads hide their cost.
  Simplicity is the default; a sizeable measured speedup can justify extra
  complexity. Meeting a ratio is no reason to keep avoidable overhead.
- Battery life is broadly comparable with Linux for agreed everyday workloads.

## Reproducibility and change

Published OS contents are reproducible from pinned inputs, excluding signatures
and machine-specific installation data. Fast incremental rebuilds and targeted
restarts suffice; a session restart or reboot is acceptable when needed.

## Deferred

Hibernation, snapshots, compression, multi-drive pooling and mirroring,
independent per-user encryption, Linux binaries, hosted virtual machines,
native containers, additional package catalogues, browser calls, DRM, VPNs,
captive portals, GPU rendering and video acceleration, translations, screen
readers, adjustable text size, built-in file search, general desktop automation
and transparent app recovery after a desktop failure. Pooling, mirroring and
containers remain architecturally possible without being built now.
