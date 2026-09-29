---
status: owner
kind: question
opened: 2026-09-29
---

# How a loader change reaches the T14 without Ubuntu is the owner's

`update` writes a slot and never the ESP's loader. Once stage 2 of
`issues/hardware/the-t14-reboots-through-ubuntu-for-every-test.md` takes
Ubuntu out of the loop, a loader change reaches the T14 only as its stick
pulled and flashed on the Mac through `diag/flash.sh`, whose `diskutil`,
`plutil` and `dd` the rules refuse
(`issues/build/the-owners-flash-script-runs-diskutil.md`). The loader track's
stages 8 and 9
(`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`)
both change the loader after that point. Two orders:

- **The loader updates itself first, and Ubuntu stays until then.** `update`
  writes a signed loader to a second ESP file; the running loader verifies it
  and chain-loads it once, and only a loader that has booted a good slot
  becomes the one the firmware boots. Stage 2 waits on it, and until it lands
  a loader change is flashed through Ubuntu as today.
- **Ubuntu goes at stage 2, and loader changes are flashed by hand until a
  self-update exists.** Every loader change until then is the stick pulled and
  written on the Mac.

*Recommended:* the first, because the second puts a hand and a script the
rules refuse in the path of every loader change.

**Exit**: the owner rules.
