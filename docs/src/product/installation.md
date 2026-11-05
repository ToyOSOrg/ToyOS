# Installation and updates

## Installing

Installation media boot to the desktop, and a desktop entry starts a graphical
installer. Installation and first-account setup work offline. A dedicated drive
is enough; nothing erases unrelated storage without authorisation. The
installer leaves every partition it did not create untouched and adds one boot
entry; on a shared EFI system partition it writes only its own directory, never
another OS's, so that OS keeps booting through the firmware's menu.

Documented firmware key enrolment is acceptable; after it, boot is verified.
Recovery from a broken installation may initially be manual.

## The image

The image is small: the desktop, settings, file manager, terminal, installer,
package tools, and the drivers essential for booting, installing and
networking on the release machines. It bundles no editor, SSH, browser, coding
agent, development toolchain or microcode payload; those are downloaded when
wanted. Compatible driver and microcode packages are recommended, and installing
one is not a grant of device authority.

## Packages

- **A package is a program, whole**: self-contained, fixed at build time, with
  its own libraries. There are no library packages, no dependency solver and no
  install, update or uninstall scripts. A fix in what a package carries is a
  rebuild of that package.
- **An installed version is immutable.** It is unpacked privately and committed
  in one step; nothing edits a committed package.
- **App setup and private-data migration happen when the app runs.** A failed
  migration stops with a clear error and keeps the remaining data for repair.
- **Sources**: one ToyOS catalogue, verified by a signed repository whose root
  is pinned in the image, plus manually installed package files. An unsigned
  package needs explicit approval and shows an unverified publisher. A native
  unpackaged program may run; approvals it receives last one run.
- **Who installs**: a user approves installing for themselves; installing for
  every user and changing the system need system authority.
- **Removal**: quit the app first. Its saved permissions are forgotten, the user
  chooses to keep or delete its private data, and documents held elsewhere are
  untouched.

## Updates

Verified updates are prepared automatically. An OS update activates at a
voluntary reboot, which the user may postpone; an app update activates while the
app is stopped, with a per-app pause. There is one stable OS
release. A failed download or staging leaves the existing installation
intact. Snapshots of app updates and a built-in backup
product are not provided; backups belong to separately installed applications.

How the machine boots an update and refuses an image it should not run is
[Boot and updates](../architecture/boot.md).
