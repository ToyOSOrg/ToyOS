# The desktop and its hardware

## Machines

Release hardware is the two bench laptops: a ThinkPad T14 Gen 2 (Intel Core
i5-1135G7) and a Lenovo 82A2 (AMD Ryzen 7 4800U). Every device actually fitted
to them works once the applicable packages are installed. AArch64 is a
development target, not a physical release gate.

Display is one monitor with software rendering. Userland display drivers may
handle presentation, panel power and resume; the firmware's boot display and
an unaccelerated virtual GPU remain supported. Hardware rendering and decoding
are not assumed. Multiple monitors, high and variable refresh rates are later
goals.

Power: ordinary whole-machine sleep, configurable lid-close behaviour, and
visible, overridable keep-awake requests from apps. Idle CPU sleep is a
separate mechanism (see [Scheduling and power](../architecture/scheduling.md)).

## The desktop

The desktop is part of ToyOS and ships in the image as a complete desktop; it
is never an installable package. It includes the desktop shell, settings, a
file manager and a terminal. Graphical resource inspection and app-stop
controls stay usable under supported resource pressure.

- **Look**: the familiar shape every desktop shares: a taskbar holding pinned
  and running apps together, one start menu with search, clear window chrome,
  and snapping a window to an edge or half the screen. Minimal and
  discoverable, with no glass or blur, because rendering is on the CPU.
- **Window management**: floating and tiling.
- **Toolkit**: ToyOS's own desktop software uses iced alone, on plain upstream
  iced. ToyOS writes its own desktop environment rather than porting another
  one.
- **Language**: an English interface and keyboard navigation at first.
  Language, keyboard layout, regional formats and time zone are independent
  choices. Compact layout, time-zone and format data are bundled; larger extras
  are downloaded.

## Apps in a session

- Programs may run without windows.
- An app can be launched to outlive the shell or terminal that started it while
  remaining part of the login session. Ordinary commands keep helper
  semantics; independent launch is explicit.
- Starting an app automatically at sign-in needs a remembered approval.
- Users choose default apps for files and links.
- Deleting from the file manager moves to a recoverable Trash; ordinary
  programmatic deletion is unchanged.
- Read/write USB file exchange with Windows and macOS works through a common
  format; exFAT is the candidate, subject to implementation and licensing.

## More than one user

Switching users keeps the previous sessions running. Signing out ends that
session's work; blocked or unsaved work offers cancel or an explicit forced
sign-out. Only the active session controls the screen, keyboard, camera and
microphone: switching suspends an inactive session's capture while its
computation continues, and pauses or mutes its playback by default until the
user returns.
