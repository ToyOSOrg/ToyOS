#!/bin/bash
# gate.sh <worktree> <out-dir> [root]
# Runs `cargo run -- --ci host` in <worktree> as the non-root user `gate`
# (or as root with a third argument `root`), one gate at a time on this host.
# Writes <out-dir>/host{,-root}.log, .exit (EXIT=<n>) and .head (the commit run).
set -u
WT=$(realpath "$1"); OUT=$(realpath -m "$2"); AS=${3:-gate}
PRIMARY=/home/user/ToyOS
SCRATCH=/tmp/claude-0/-home-user-ToyOS/4f763767-580b-5f57-ae09-9c2534647a13/scratchpad
mkdir -p "$OUT"
SUFFIX=$([ "$AS" = root ] && echo -root || echo "")

# One full build at a time on this host.
exec 9>"$SCRATCH/gate.lock"
echo "gate: waiting for the host's gate lock ($WT)"
flock 9
echo "gate: running in $WT as $AS"

# A linked worktree's rust/ is made the way src/sysroot.rs fork_checkout makes
# it; done here as root because the primary's fork repository is root's.
if [ ! -e "$WT/rust/.git" ]; then
  PIN=$(git -C "$WT" ls-tree HEAD rust | awk '{print $3}')
  rmdir "$WT/rust" 2>/dev/null
  git -C "$PRIMARY/rust" worktree add --detach "$WT/rust" "$PIN" > "$OUT/fork-checkout.log" 2>&1 || { echo "gate: fork checkout failed, see $OUT/fork-checkout.log"; exit 2; }
  git -C "$WT/rust" submodule update --init --depth 1 library/backtrace >> "$OUT/fork-checkout.log" 2>&1 || { echo "gate: backtrace checkout failed"; exit 2; }
fi

# Free disk: under 12 GB, other worktrees' build output goes (no build runs: we hold the lock).
AVAIL=$(df --output=avail -BG / | tail -n 1 | tr -dc 0-9)
if [ "$AVAIL" -lt 20 ]; then
  for t in /home/user/*/target /home/user/*/userland/target /home/user/*/kernel/target /home/user/*/toyos/target /home/user/*/bootloader/target; do
    case "$t" in "$WT"/*) ;; *) [ -d "$t" ] && rm -rf "$t" && echo "gate: freed $t";; esac
  done
fi

git -C "$WT" rev-parse HEAD > "$OUT/host$SUFFIX.head"
git -C "$WT" status --porcelain --ignore-submodules=none > "$OUT/host$SUFFIX.status"
if [ "$AS" = root ]; then
  ( cd "$WT" && cargo run -- --ci host ) > "$OUT/host$SUFFIX.log" 2>&1
  echo "EXIT=$?" > "$OUT/host$SUFFIX.exit"
else
  chown -R gate:gate "$WT"
  runuser -u gate -- bash -c ". /home/gate/.gate-env; cd '$WT' && cargo run -- --ci host" > "$OUT/host$SUFFIX.log" 2>&1
  echo "EXIT=$?" > "$OUT/host$SUFFIX.exit"
fi
echo "gate: $(cat "$OUT/host$SUFFIX.exit") at $(cat "$OUT/host$SUFFIX.head") as $AS; log $OUT/host$SUFFIX.log"
tail -n 5 "$OUT/host$SUFFIX.log"

# The disk holds one gate's build output at a time: once the result is
# recorded, this worktree's target dirs go (the next gate rebuilds them).
find "$WT" -maxdepth 3 -type d -name target -prune -exec rm -rf {} + 2>/dev/null
echo "gate: removed $WT's target dirs; free $(df --output=avail -BG / | tail -n 1 | tr -d ' ')"
