---
status: open
kind: defect
opened: 2026-08-24
---

# `/bin/doom` links on this dev host and not on a fresh macOS one, and the difference is which `ar` is in `PATH`

The first finding `.github/workflows/portability.yml` has ever produced, from
the first run of that workflow that got past its own setup (dispatch
32749539353, 2026-08-24, `macos` job, 43m11s). Same commit, same run: the
`linux` job built everything green and macOS died in the userland build.

```
   Compiling compositor v0.1.0 (/Users/runner/work/ToyOS/ToyOS/userland/compositor)
error: linking with `toyos-ld` failed: exit status: 1
  = note: toyos-ld: undefined symbol: DG_ScreenBuffer
error: could not compile `doom` (bin "doom") due to 1 previous error
thread 'main' (16602) panicked at src/build.rs:363:9:
cargo build failed in /Users/runner/work/ToyOS/ToyOS/userland
```

`DG_ScreenBuffer` is defined in `doomgeneric.c`, which
`userland/doom/build.rs` compiles with `toyos-cc` and hands to
`cc::Build::compile("doomgeneric")`. That call archives the objects with
whatever archiver the `cc` crate resolves — a host binary neither the
dependency doctrine declares nor anything in this repository names.

**Measured 2026-08-24, and it is the archive format.** The working
`libdoomgeneric.a` on this dev host (1,952,626 bytes, built 2026-08-20) begins
`!<arch>\n` followed by the member name `/` — the **GNU** symbol table. It was
written by `/opt/homebrew/opt/binutils/bin/ar`, GNU ar 2.47.20260726, which is
what `which ar` answers here because Homebrew's binutils is installed and
earlier in `PATH`. `macos-latest` has no binutils: its `ar` is `/usr/bin/ar`,
Apple's cctools archiver (118,640 bytes, the same inode as `/usr/bin/libtool`),
which writes **BSD**-format archives — `__.SYMDEF` for the symbol table and
`#1/<len>` extended member names whose real bytes live at the head of the
member data, NUL-padded.

So the two hosts do not hand `toyos-ld` the same container, and only one of the
two has ever been tested.

**The named suspect, unverified.** `toyos-ld/src/collect.rs`'s
`extract_archive` selects members by name:

```rust
let member_name = String::from_utf8_lossy(member.name()).to_string();
if !member_name.ends_with(".o") {
    continue;
}
```

A BSD `#1/` name carrying its NUL padding does not end in `.o`, and a member
dropped there is a definition that silently is not there — which is the shape
of this failure. Nobody has confirmed that is what the `object` crate returns
for a cctools archive; a fix starts by writing one on a stock macOS host and
reading the member names back, not by loosening the filter.

**The dependency finding is the larger half.** The build reaches for a host
`ar` that no rule declares and no gate names, and on macOS the answer depends
on software the doctrine does not sanction being installed. `cc` for every host
link is the standing exception root `CLAUDE.md` records; the archiver is not
that, and it is not in the exception list.
