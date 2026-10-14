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

**Exit**: a new patch for a machine arrives as a file on ROOT through the
package path, and the loader hands it over on the next boot without a new
image.
