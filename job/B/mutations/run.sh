#!/bin/bash
# Apply each mutation patch to the worktree, build and run the toyos crate's
# host tests as root, record both exit codes, and restore the tree.
set -u
WT=/home/user/toyos-handlesend
M=/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/B/mutations
TEST=(cargo test --manifest-path toyos/Cargo.toml --target x86_64-unknown-linux-gnu)
OUT=$M/results.txt
: > "$OUT"
echo "head $(git -C "$WT" rev-parse HEAD)" >> "$OUT"
for p in "$M"/m*.patch; do
  name=$(basename "$p" .patch)
  if [ -n "$(git -C "$WT" status --porcelain --ignore-submodules=none)" ]; then
    echo "$name: tree not clean before apply; abort" >> "$OUT"; exit 2
  fi
  git -C "$WT" apply --check "$p" || { echo "$name: apply --check failed; abort" >> "$OUT"; exit 2; }
  git -C "$WT" apply "$p"
  (cd "$WT" && "${TEST[@]}" --no-run) > "$M/$name.build.log" 2>&1
  build=$?
  (cd "$WT" && "${TEST[@]}") > "$M/$name.log" 2>&1
  run=$?
  git -C "$WT" apply -R "$p"
  clean=$(git -C "$WT" status --porcelain --ignore-submodules=none | wc -l)
  echo "$name: BUILD EXIT=$build RUN EXIT=$run dirty-lines-after-restore=$clean" >> "$OUT"
  [ "$clean" -eq 0 ] || { echo "$name: restore left the tree dirty; abort" >> "$OUT"; exit 2; }
done
(cd "$WT" && "${TEST[@]}") > "$M/restored.log" 2>&1
echo "restored tree: RUN EXIT=$? dirty-lines=$(git -C "$WT" status --porcelain --ignore-submodules=none | wc -l)" >> "$OUT"
echo done >> "$OUT"
