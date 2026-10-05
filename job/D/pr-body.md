## What this is

Closes `issues/a-file-servers-shares-are-per-instance-and-one-session-launches-instances.md`
(deleted; `git grep` finds no citation of its slug, path or bare name left). It advances stage 1 of
`issues/every-program-sees-only-the-files-it-was-given.md`, over #729's per-instance shares.

**Before:** the supervisor minted a fresh grant number on every start, and a file server gave each
number a quarter of each bound. A session took a whole server by launching four programs, and a
shell launches shells.

**After:** a grant names the session it was minted in. Every start in a session, through every
launch made in it and every child spawned directly, spends that session's one share. Only a launch
by a `login` row opens a new session.

## The change, per decision

### Sessions get numbers (`toyos-manifest/src/launch.rs`)
- `Session::Login` carries a `Login` number. Its field is private: only `Sessions::open` makes one,
  and `Authority::decode` gives one back from a badge.
- `Session::share` is what a grant names:
  - 0 (`SUPERVISOR_SHARE`) is the supervisor's own files;
  - 1 is the machine's session;
  - 2 and up are login sessions.
  - Being distinct is the type's job: a `Login` below 2 cannot be built, and decode refuses one.
- The launcher's badge (`Authority`) is a kind byte, then the login number (8 bytes) for a login
  session, then the row. That is 1 + 8 + 32 ≤ `MAX_BADGE` 64, which a `const` assert checks. This
  needs no kernel or ABI change.
- `may_start` returns `Starts::In(caller's session)`, or `Starts::Opening` for a `login` caller.
  The supervisor opens the session only once the launch has been prepared. It decides this on its
  file worker thread, so `may_start` stays pure and takes no counter.
- The `OutsideLogin` rule is unchanged: a login-only row is refused only when the launch would run
  in the machine's session.

### The supervisor (`userland/supervisor/src/main.rs`)
- `Grants::view(program, session)` mints each directory's grant with `session.share()`.
  - Every boot row and every restart or swap gets the machine's share.
  - A launch gets its caller's share, or a newly opened one.
- The per-start counter (`next: Cell<u64>`) and `SUPERVISOR_INSTANCE` are deleted.
- `build_namespace` takes the minted view instead of `Grants`.

### The grant (`toyos/src/fs.rs`)
- `Grant::instance` is renamed `session`.
- `GRANT_VERSION` goes from 1 to 2, because the number now means something else. A swap replaces a
  file server and not the supervisor. Without the bump, a server of this build reading an older
  supervisor's per-start numbers would silently enforce per-start shares again. With it, either
  side refuses the other's grants by name, which is what the version is documented to do.
- Being in `toyos/src`, this is a new sysroot identity. CI builds that sysroot.

### The fileserver (`userland/fileserver/src/main.rs`)
- Every share is keyed by `session`. The bounds and the quarter are unchanged.
- The module header's invariant now reads "One session cannot take the server". The issue's
  citation is removed.

## Tiers

- **Type.** Disjoint shares are a `Login` the supervisor alone numbers. Nothing tests what that
  refuses.
- **Host:** `toyos-manifest` launch tests.
  - The `may_start` table now says which arm opens a session: 24 rows, both kinds of caller,
    every target.
  - New: `a_sessions_launches_spend_its_one_share`. Along the shipping desktop's chain (compositor
    → terminal → shell → 8 nested shells, twice), every launch down one chain has the share its
    terminal's launch opened. Two compositor launches never share one, and neither shares the
    machine's or the supervisor's.
  - Authority codec: a login number round-trips, including `u64::MAX`. A login number of 0 or 1,
    or one cut short, is refused.
- **Metal row `fs_share` on `tests/proctreecase`, rewritten.** It is in `RUST_SKIP`, so it is not a
  QEMU guest test. Arms, in order:
  1. Each DATA directory is its grant's root. Unchanged.
  2. This job, in the machine's session, takes every stream until refused. Then a shell it
     launches, in this session, has its redirect's stream refused: the file exists and is empty.
     **New.**
  3. It takes every served connection until refused. Then a shell it launches has its redirect's
     connection refused: the file is `NotFound`, and the child's echo came back on the shell's
     output instead. **New.**
  4. A shell it launches launches another shell. `proctreecase`'s shell row is `login` and now
     also lists `shell`, so the second shell opens a login session. That session connects to
     `/home`, streams `toybox echo` into a file, and the bytes are read back. This is the issue's
     exit, stated exactly: one session holding all it may, through its launches, leaves another
     session's first connection and first stream answered.
  5. The handshake and window-leak arms. Unchanged, now per session.
- **Why no host test reaches the boundary.** The shares are decided in the fileserver's live poll
  loop, on grants the kernel stamped. The session is minted by the live supervisor into a badge the
  kernel stamps on a launcher connection. The arms' launches go through the live launcher. #729's
  body gives the same reasons for the arms it added.
- **Why the shells run from `/`.** That cwd is the kernel's, not a file server's, so a redirect is
  the only file each shell opens. Std judges a served cwd with a `metadata` call, which would be a
  connection of its own.

## High-risk: negative control, mutations, oracles

**What ran here (host).** Each mutation was applied as a checked patch, run with
`cargo test -p toyos-manifest`, and reversed. `git status --porcelain --ignore-submodules=none`
printed nothing after each. The patches are in `mutations/host-*.patch`, and the logs sit beside
them in the job directory.

| patch | EXIT | red |
|---|---|---|
| fix, at 750af802 | 0 | `24 passed` |
| host-1: every launch opens a session. This is main's per-start share, written in the new code: the host-tier control | 101 | `a shell's launches left its session` (left 11, right 2); table row 0 `Ok(Opening)` ≠ `Ok(In(Machine))` |
| host-2: `Sessions::open` never advances | 101 | `two launches of a login row share one session` |
| host-3: decode takes any login number | 101 | `what_encode_cannot_have_written_is_refused`: `Login(Login(0))` decoded |
| host-4: the machine's share is the supervisor's | 101 | `a_sessions_launches_spend_its_one_share`: `0 == 0` |

**Why host-1 stands in for a whole revert.** A whole revert removes the host test with the code it
tests, so on the host the base's rule is expressed as host-1.

**Owed to the T14 (the orchestrator's), not run here.** This host has no ToyOS toolchain, no KVM and
no declared QEMU, so it cannot build an image or stage a metal readback. Every patch below passes
`git apply --check` where it is meant to apply.

- **Negative control.** `mutations/metal-control-base-plus-this-branchs-tests.patch` is this
  branch's whole `tests/` delta, applied onto `origin/main` f260e0b9 with the rest of the change
  reverted.
  - Expected: `fs_share` red on arms 2 and 3. On main, each launched shell is an instance of its
    own, so its stream and its connection are answered: `a shell launched in this session streamed
    Ok("answered")` and `… made /home/fs_share/same_connection`.
  - Arm 4 is green on main, because main's fresh instance passes it too.
- **Metal mutations, each expected red on arm 4 (`another session's stream wrote …`):**
  - `metal-1-every-start-minted-the-machines-share.patch`: the supervisor ignores the launch's
    session;
  - `metal-2-stream-share-counts-every-session.patch`;
  - `metal-3-served-share-counts-every-session.patch`.
- **Stage and judge:** `cargo test --test toyos-build -- --metal --metal-readback <dir>/fs_share fs_share`
  from this head, then the same for the control and each mutation.

**Independent oracles.**
- **A recorded real failure:** the closed issue's scenario, one session launching instances to take
  DATA's server. Arms 2 and 3 are that scenario, and the control reproduces it on main.
- **Real hardware:** the T14 row above. It is owed, and none is claimed.

## Gates at 750af802

| gate | exit | log |
|---|---|---|
| `cargo run -- --ci host` (via `gate.sh`, non-root) | 0, `Host: 75 step(s), all green`; `host.head` 750af80283fd, `host.status` empty | `host.log` in the job directory D/ |
| `cargo test -p toyos-manifest` | 0, `24 passed` | `mutations/host-0-fix.log` |
| SDK grant tests (`cargo test --manifest-path toyos/Cargo.toml grant`) | 0, `3 passed` | run while iterating |
| guest suite / `cargo test` | not run: this host builds no image | owed to CI's `guest / suite`. No QEMU guest test changes: `fs_share` is a metal row in `RUST_SKIP`. But `toyos/src` changed, so every guest test rides the new sysroot. |
| T14 `fs_share` | not run | owed (above) |

**The supervisor and `fs_share` have no host build.** Because of that, each was type-checked on the
host while iterating:
- the supervisor with its ToyOS-only `std` extensions stubbed. A planted type error in the launch
  path was caught (E0308), so the check sees that code;
- `fs_share` against the SDK in a scratch crate;
- the fileserver binary for the Linux target.

None of these is evidence of behaviour.

## Unsure

- **Four sessions still take a server, and sessions are cheap.** Every launch a `login` row makes
  opens one: each taskbar launch, and each sshserver client. A session can reach sshserver through
  the shell's and toybox's `starts`. Filed with an exit as
  `issues/four-login-sessions-at-their-shares-take-a-file-server.md`, under the track's stage 4.
- **Each taskbar launch is its own session.** That is what `launch.rs` already said ("a launch by a
  row marked `login` opens a login session"), and this branch only gives each one a number. The
  owner ruled "the local desktop is a login session", and per-session `/tmp` will make the
  difference visible: two terminals would have two `/tmp`. Whether the desktop is one session is
  not decided here.
- **The machine's session is one share for every boot service** and for everything a non-`login`
  boot row launches (test-runner's jobs). #729's measurements bound that: DATA never served more
  than 5 at once machine-wide, and the desktop at rest holds 3. A parallel build is still the
  unmeasured load.

## Net lines (`git diff --shortstat origin/main...HEAD`: 9 files, +330 −191)

| part | lines |
|---|---|
| **Production** | **net +68** |
| `toyos-manifest` launch (sessions, `Starts`, codec) | +72 net |
| SDK grant (rename, version) | +15 −15 |
| fileserver (rename) | +22 −22 |
| supervisor | +28 −32 |
| **Tests** | net +76: launch tests +44, `fs_share` +30, harness and config +2 |
| **Issues** | +32 −37 |

Production grows because a session needs a number, a counter that alone mints one, and room for it
in the launcher's badge.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_015tYBoMwh9xUcBBLG35wTFr
