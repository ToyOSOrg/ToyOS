---
status: open
kind: track
opened: 2026-09-26
---

# Where everything lives, and how a program finds it

The owner ruled this layout on 2026-09-26. This file is where it is recorded:
`issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md` says what
backs each name, and
`issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md` and
`issues/isolation/every-program-sees-only-the-files-it-was-given.md` say who
may reach it.

## The tree

```
/system                read-only, signed: bin, lib, etc, and
                       share/{fonts, icons, themes, sounds, backgrounds, licenses, doc}
/config                machine settings the owner changes; a file here overrides /system/etc
/state/<program>       a system service's private persistent data
/apps/<name>           installed packages; no layout is required inside one
/home/<user>           Desktop Documents Downloads Music Pictures Videos Fonts Apps/<name>/
/boot                  bootloader, kernel, kernel arguments; the updater's alone
/log  /media           logs; foreign volumes, one /media/<label> each
/tmp                   private per program
```

## The rules

- **No drive letters, no `/usr`, `/var`, `/opt`, `/dev`, `/proc` or `/sys`**:
  devices and processes are capabilities and syscalls here, not files.
- **No dotfile and no hidden folder anywhere in the layout.** A third-party
  program that hard-codes `~/.something` may still create one, but only inside
  its own app folder.
- **English names, never translated.**
- **A path means the same file in every view.** `/tmp` is the one exception:
  it is private per program. A view hides names and never renames one.
- **A program learns a location from an environment variable init sets**
  (`HOME` and those like it). A location grants nothing, because the view
  decides what a program can reach. On ToyOS `std::env::home_dir()` is `$HOME`
  or `None`, with no fallback, and `temp_dir()` is `/tmp`.
- **`/home/<user>/Apps/<name>` is one app's private data for that user**, and
  it is that app's `HOME`. Inside it ToyOS answers config, data, cache and
  state as the visible `Config`, `Data`, `Cache` and `State`.
- **Fonts**: the system's are in `/system/share/fonts`, a user's in
  `/home/<user>/Fonts`. sans-serif and system-ui are Open Sans, monospace is
  JetBrains Mono, and serif is empty until a licence-clean one is chosen.
- **The time zone is machine-wide in `/config`.** The keyboard layout and the
  language are machine defaults in `/config`, and per-user overrides come with
  the users track.
- **The dev image's user is `toy`.** There is no `root` user and no
  `/home/root`.
- An app finds its own files next to its binary (`current_exe()`).

## Stages

1. **Everything that needs no isolation.** `/config` and `/state` are DATA
   names; init makes `/home/toy`, its eight folders, and each service's
   `/state/<name>`, and it sets `HOME` from each row (`service = true` in
   `system.toml`); the kernel makes no home; the keyboard layout is
   `/config/keyboard-layout`; sshd keeps its identity and key list in
   `/state/sshd`; the shell's history is `$HOME/Apps/shell/State/history`;
   std's `home_dir` reads `$HOME`; JetBrains Mono ships as a TTF with its OFL
   text. **Exit**: `layout_fresh_boot` is green.
2. **An app's `HOME` is its folder.** init launches an app from `/apps` (and
   each desktop app in the image) with `HOME=/home/<user>/Apps/<name>`, makes
   the folder and its `Config Data Cache State`, and puts nothing else of the
   home in that app's view. It needs
   `issues/isolation/every-program-sees-only-the-files-it-was-given.md` stage 2.
   **Exit**: a launched app's `home_dir()` is its folder, and it cannot name
   another app's folder.
3. **Users.** The users track creates `/home/<user>` and its folders from a
   login row, and `toy` stops being a constant in `toyos-manifest`.
   **Exit**: init names no user.
4. **The upstream arms**, none opened until the owner opens it:
   - fontdb and fontique scan `/system/share/fonts` and `$HOME/Fonts` from the
     start, and fontique maps the families above. **Exit**: both prepared
     branches carry both folders and the family table.
   - dirs-sys answers `home_dir` from std, and the six user folders and the
     four `Config Data Cache State` directories under `$HOME` by their names
     here. **Exit**: `dirs` and `directories` build for ToyOS and answer this
     layout.
   - home answers from std. **Exit**: `home` builds for ToyOS.
   - tempfile takes the unix create-then-unlink path once std's
     `create_new` and unlink-while-open are measured on ToyOS. **Exit**:
     `tempfile` makes files on ToyOS.
   - sys-locale reads the language init sets from `/config`. **Exit**:
     `sys-locale` answers on ToyOS.
   - rustls-native-certs reads a CA bundle under `/system/etc`, once something
     needs native roots. **Exit**: the first native-roots consumer runs.

## Not moved

`/log/lease.txt` stays on `/log`. It is not netd's lease: it is the
`--exit-with-lease` bench report the metal loop reads off the stick's FAT log
volume, as it reads `/log/metal-*.bin`, and the DATA volume is not readable
there. It goes with
`issues/diagnostics/the-lanleasecase-boot-is-a-third-t14-flash-for-one-exit-code.md`.
