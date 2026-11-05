---
status: open
kind: track
opened: 2026-09-26
---

# Where everything lives, and how a program finds it

The file tree's layout and naming rules, ruled by the owner on 2026-09-26 and
amended for `/tmp` on 2026-10-03, built in the stages below.
`issues/storage-is-layers-and-a-role-is-a-filesystem.md` builds what backs
each name, and `issues/a-user-is-a-home-tree-and-a-login-row.md` and
`issues/every-program-sees-only-the-files-it-was-given.md` who may reach it.

## Stages

1. **Everything that needs no isolation.** `/config` and `/state` are DATA
   names; init makes `/home/toy`, its eight folders, and each service's and
   `state = true` row's `/state/<name>`, the only part of `/state` that row's
   view holds, and it sets `HOME` from each row (`service = true` in
   `system.toml`, which a row that serves a port must say); a program no row
   names carries the `HOME` init answers for it, never its spawner's own;
   the kernel makes no home, and lists a directory it carries; the keyboard
   layout is `/config/keyboard-layout`; sshd keeps its identity and key list
   in `/state/sshd`; the shell's history is in `Apps/shell/State`; std's
   `home_dir` reads `$HOME`. **Exit**: `layout_fresh_boot` is green.
2. **An app's `HOME` is its folder.** init launches an app from `/apps` (and
   each desktop app in the image) with `HOME=/home/<user>/Apps/<name>`, makes
   the folder and its `Config Data Cache State`, and puts nothing else of the
   home in that app's view. It needs
   `issues/every-program-sees-only-the-files-it-was-given.md` stage 2.
   **Exit**: a launched app's `home_dir()` is its folder, it cannot name
   another app's folder, and the shell's history is still
   `/home/<user>/Apps/shell/State/history` (`OWN_FOLDER` in `system/shell`).
   Built for a package launched from `/apps` (`toyos_manifest::Program::view`,
   judged by the `app_view` metal row). Each desktop app in the image still
   runs with the session's `HOME` and the whole tree but `/state`: its row
   declares no view until that isolation stage gives every row one.
3. **Users.** The users track creates `/home/<user>` and its folders from a
   login row, and `toy` stops being a constant in `toyos-manifest`.
   **Exit**: init names no user.
4. **The time zone.** One file in `/config` names the machine's zone, one
   program writes it, and local time is read through it. **Exit**: a guest
   test sets the zone and reads its local time back.
5. **The language.** One file in `/config` names the machine's default
   language, one program writes it, and init sets it on every program it
   starts. **Exit**: a guest test sets the language and a program init starts
   reads it back.
6. **Fonts ship as files.** JetBrains Mono and Open Sans ship as TTFs under
   `/system/share/fonts` from `assets/fonts/`, the console's raster stays its
   own asset, and each OFL text ships once, under `/system/share/licenses`.
   **Exit**: an image carries both families there and their licence texts
   under `/system/share/licenses`, and none under `/system/share/fonts`.
7. **The upstream arms**, none opened until the owner opens it:
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
