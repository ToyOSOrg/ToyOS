---
status: open
kind: defect
opened: 2026-10-09
---

# An installed gbae browses to no ROM

gbae (`Japabu/gbae` at `bfe8dabf8`), started with no ROM, opens its own file
menu on `std::env::current_dir()` (`src/main.rs:471`) and walks it with
`std::fs::read_dir` (`src/menu.rs:308`). A package's view is its own
`/apps/<name>` read-only and its own `/home/toy/Apps/<name>`
(`toyos_manifest::Program::view`), and nothing else a file server serves, so
that menu reaches no ROM anywhere. The package track records the opposite:
"It lists a directory itself and reads the ROM the user picks out of it"
(`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`).

Evidence, in a QEMU guest on `tests/proctreecase`, a package launched from
`/apps` running gbae's `list_directory` verbatim and its loads, with ROMs at
`/home/toy/Downloads` and in the package's own `Data` folder:

- From cwd `/`, the compositor's own, which its launch carries (read from
  the code, not measured), the menu lists `/`'s nine
  mount points; `/system` lists `bin/` and `etc/`, which hold no ROM, and
  every other one lists only `../`, `/home` and `/home/toy` included: no ROM
  is browsable.
- From cwd `/home/toy`: the same.
- A ROM given by path loads from the package's own folder and not from
  `/home/toy/Downloads` (`NotFound`).
- Its config, `$HOME/.config/gbae/config`, is written and read back in its
  own folder.

**Exit**: an installed gbae, started from the desktop, loads a ROM the user
picked from outside its own folder. The designed answer is the file picker,
which hands an app the one file the user chose: the package track's stage 6
(an app's rights are its request, the user's grant and the image's ceiling),
and the isolation track's "Sharing is granted, never reached"
(`issues/every-program-sees-only-the-files-it-was-given.md`). Until then gbae
plays only a ROM placed in its own folder and given on its command line.
