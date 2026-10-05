#!/bin/bash
# The census judge's host test at the branch head, then under each control
# patch: applied checked, run, restored in the same script.
set -u
WT=/home/user/toyos-irqcensus
M=/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/C/mutations
L=/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/C/logs/r2
cd "$WT" || exit 9
echo "head $(git rev-parse HEAD)"
cargo test --test toyos-checks irq_census_verdict > "$L/control-0-head.log" 2>&1
echo "head: EXIT=$?"
for p in "$M"/irq-census-*.patch; do
  name=$(basename "$p" .patch)
  git apply --check "$p" || { echo "$name: does not apply"; exit 9; }
  git apply "$p"
  cargo test --test toyos-checks irq_census_verdict > "$L/control-$name.log" 2>&1
  echo "$name: EXIT=$?"
  git apply -R "$p"
done
echo "status after restore: [$(git status --porcelain --ignore-submodules=none)]"
