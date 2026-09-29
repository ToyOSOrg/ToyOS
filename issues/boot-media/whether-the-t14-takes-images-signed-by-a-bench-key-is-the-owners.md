---
status: owner
kind: question
opened: 2026-09-29
---

# Whether the T14 takes images signed by a bench key is the owner's

The owner's ruling is that the machine installs nothing the owner did not sign
(`issues/boot-media/the-machine-updates-itself-without-ubuntu.md:9-13`). Once
the T14 takes every image by `update`, which stage 2 of
`issues/hardware/the-t14-reboots-through-ubuntu-for-every-test.md` makes it do
and which never replaces the loader, it boots only what its stick's loader was
built to verify. Every checkout signs with a throwaway key of its own
(`src/signing.rs`' `THROWAWAY_FILE`), so the stick would take images from the
one checkout that flashed it; signing test images with the owner's key would
put that key in every agent's build.

*Recommended:* a bench key, made once and kept outside every checkout, which
the T14's loader verifies and every checkout signs its T14 images with: this
test laptop's one exception to the ruling. Nothing in the tree makes one:
`--signing-key-new` mints the owner's key at `owner_key_path()` and refuses to
replace it (`src/main.rs:143-144`, `src/signing.rs:195-236`), so a bench key
needs a mint and a signing path of its own.

Stage 2 of that track waits on this.

**Exit**: the owner rules.
