# This host (read before you build)

- A cloud container: 4 cores, 15 GiB RAM, ~26 GB free disk shared by every worktree. You run as root. No KVM.
  QEMU 8.2.2 is installed only so the build system's tool check passes; `.github/qemu-version` declares 11.1.1.
- **No guest tests here and no `--build-only`.** Both need the ToyOS toolchain (an LLVM build of hours on 4
  cores) and the declared QEMU. CI's `guest / suite` (KVM, QEMU 11.1.1) runs them once the orchestrator readies
  the pull request. Your PR body names every guest test your change reaches and what only that suite can show;
  never claim a guest result you did not see.
- **The host gate runs only through** `/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/gate.sh <your worktree> <your job dir>`
  (add a third argument `root` to run it as root). It runs `cargo run -- --ci host` as the non-root user `gate`,
  one gate at a time on this host (it waits for the lock), and writes `host.log`, `host.exit` (EXIT=n), `host.head`
  and `host.status` into your job dir. It chowns your worktree to `gate`; root still edits it, and git is told
  every directory is safe. Run it in the background, wait on `host.exit` existing, and commit before you gate:
  the head it records is the head your evidence is for.
- While iterating, `cargo test -p <crate>` / `cargo clippy -p <crate>` as root with the stock toolchain are fine
  and fast; they are not evidence.
- Your job dir (`/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/<task>/`) holds your logs, mutation patches and
  results; nothing evidence rests on goes only to `/tmp`.
- **Pull requests:** `gh` is not usable here. Push your branch (`git push -u origin <local>:<remote branch the brief
  names>`), and write the PR title to `<job dir>/pr-title.txt` and the body to `<job dir>/pr-body.md`; the
  orchestrator opens the draft PR and posts mutation patches (`<job dir>/mutations/*.patch`) as comments from them.
  The body ends with:

  ```
  🤖 Generated with [Claude Code](https://claude.com/claude-code)

  https://claude.ai/code/session_015tYBoMwh9xUcBBLG35wTFr
  ```
- Every commit message (`git commit -F <file>`) ends with these two lines:

  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_015tYBoMwh9xUcBBLG35wTFr
  ```
- Leave other worktrees (`/home/user/*`) and their target dirs alone; delete only scratch output you made.
- **Disk is the scarce resource**: one host gate fills ~15–20 GB of target dirs and the container holds ~38 GB.
  The gate deletes your worktree's target dirs after it records its result, and deletes other worktrees' target
  dirs before it runs when under 20 GB is free — so your iteration build may vanish; just rebuild. Never leave a
  second copy of a target dir anywhere.
