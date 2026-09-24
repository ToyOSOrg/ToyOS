#!/bin/sh
# Whether the diff between here and `main` touches nothing but prose — `*.md`
# anywhere, `issues/**`, `.claude/**` — so `ci.yml` and `host-tests.yml`'s
# `scope`/`abi-split` steps agree about what "prose-only" means rather than
# each guessing at the pattern.
#
# `main` and not `github.base_ref`: every workflow that calls this triggers
# `pull_request` or `merge_group` against `main` alone
# (`src/ci.rs::every_pull_request_trigger_runs_only_against_main`), so the one
# comparison this ever needs is against it.
#
# Run from the repository root, after a checkout with `fetch-depth: 0`, by any
# job that wants the answer. Prints `yes` or `no` on stdout and nothing else.
#
# Fetched unconditionally rather than trusted to already be there:
# `actions/checkout` does not leave a remote-tracking `origin/main` behind even
# at `fetch-depth: 0`, which is why `landing.yml`'s own base-ref steps fetch it
# explicitly too. Full history is what `fetch-depth: 0` buys this: a merge-base
# between `origin/main` and a shallow `HEAD` does not resolve to anything this
# reads as "prose-only".
set -eu

git fetch --quiet origin "+refs/heads/main:refs/remotes/origin/main"

# Three-dot: what this branch (or this merge group's accumulated queue)
# changed since it diverged from `main`, not what merging it back would
# change — the latter is empty on `main`'s own tip and would read every push
# to `main` as touching nothing at all.
files=$(git diff --name-only origin/main...HEAD)

# A deleted or renamed document can be one `src/redlist.rs` cites, and only
# the host suite checks that every cited issue file exists.
if [ -n "$(git diff --name-only --diff-filter=DR origin/main...HEAD)" ]; then
  echo no
  exit 0
fi

for f in $files; do
  case "$f" in
    *.md | issues/* | .claude/*) ;;
    *)
      echo no
      exit 0
      ;;
  esac
done
echo yes
