---
status: open
kind: track
opened: 2026-10-14
---

# A new microcode patch reaches a machine only in a new image

The loader hands the kernel the patch ROOT holds for the machine's CPU
(`/system/share/microcode/<vendor>/<name>`, `lib/microcode`), and ROOT holds
what `images/microcode.toml` lists for the image the build made: the
benches' patches, out of the vendor files `NOTICE` pins. So a vendor's newer
patch reaches a machine only when this tree pins the newer file and the
machine installs a new image, and a CPU no list names loads none, its loader
saying so.

Owner: the orchestrator.

Owner decision, 2026-10-18: the minimal base image carries no microcode-update
payloads. Updates come through package management, which recommends applicable
microcode for the actual CPU. CPU identification already exists in
`lib/microcode`; the package catalogue, installed/current revision handling and
recommendation path still need design.

**Exit**: a compatible authenticated patch arrives through the package path,
and the loader hands it to the kernel on the next boot without a new OS image.
Define package storage available before personal-data unlock while preserving
the verified boot and immutable-system contracts; do not assume packages can
write arbitrary files into signed ROOT. Package distribution does not move
privileged CPU-update application into an ordinary package-manager process.
