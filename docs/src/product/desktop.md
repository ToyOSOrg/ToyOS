# The desktop and its hardware

## Machines

- Release hardware is the two bench laptops: a ThinkPad T14 Gen 2 (Intel Core
  i5-1135G7) and a Lenovo 82A2 (AMD Ryzen 7 4800U).
- Every device fitted to them works once the applicable packages are
  installed.
- AArch64 is a development target, not a physical release gate.

## Displays

- The display engine is release-one scope on both laptops: the internal panel
  and external outputs, modesetting, hot-plug, EDID, and power management.
- Several outputs, extended or mirrored, each with its own mode and refresh
  rate, including high refresh and variable refresh where the display offers
  them.
- The firmware's boot display and an unaccelerated virtual GPU remain
  supported.
- Rendering is on the CPU. Video and render acceleration are not in release
  one; GPU rendering comes later through display and Vulkan drivers, not a CPU
  implementation of Vulkan.

## Power

- Ordinary whole-machine sleep and configurable lid-close behaviour.
- Visible, overridable keep-awake requests from apps.
- Idle CPU sleep is a separate mechanism
  ([Scheduling and power](../architecture/scheduling.md)).

## The desktop

- It is part of ToyOS, shipped in the image, never an installable package:
  desktop shell, settings, file manager and terminal.
- Graphical resource inspection and app-stop controls stay usable under
  supported resource pressure.
- **Look**: a taskbar holding pinned and running apps together, one start menu
  with search, clear window chrome, snapping to an edge or half the screen.
  Minimal and discoverable; no glass or blur, since rendering is on the CPU.
- **Window management**: floating and tiling.
- **Toolkit**: ToyOS's own desktop software uses iced, on plain upstream iced,
  and for now iced is the one toolkit the platform supports. ToyOS writes its
  own desktop environment rather than porting another.
- **Language**: English and keyboard navigation at first. Language, keyboard
  layout, regional formats and time zone are independent choices; compact data
  is bundled, larger extras are downloaded.

## Apps in a session

- Programs may run without windows.
- An app can be launched to outlive the shell that started it while staying in
  the login session. Ordinary commands keep helper semantics; independent
  launch is explicit.
- Starting an app at sign-in needs a remembered approval.
- Users choose default apps for files and links.
- The file manager deletes into a recoverable Trash; programmatic deletion is
  unchanged.
- Read/write USB exchange with Windows and macOS through a common format, with
  exFAT as the candidate subject to implementation and licensing.

## More than one user

- Switching users keeps earlier sessions running.
- Signing out ends that session's work; blocked or unsaved work offers cancel or
  an explicit forced sign-out.
- Only the active session controls the screen, keyboard, camera and microphone.
  An inactive session's capture is suspended while its computation continues,
  and its playback is paused or muted by default until the user returns.
