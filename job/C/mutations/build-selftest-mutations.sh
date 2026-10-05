#!/bin/bash
# Each selftest mutation applied checked, the kernel linted as `--ci host`'s
# boot-actuators shape lints it (the image the T14 row flashes is built with
# boot-actuators), and restored in the same script.
set -u
WT=/home/user/toyos-irqcensus
M=/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/C/mutations
L=/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/C/logs
ADOPTED="-W clippy::checked_conversions -W clippy::default_trait_access -W clippy::manual_midpoint -W clippy::redundant_clone -W clippy::unchecked_time_subtraction -W clippy::unnecessary_semicolon"
cd "$WT" || exit 9
echo "head $(git rev-parse HEAD)"
lint() { (cd kernel && cargo clippy --target x86_64-unknown-none --features boot-actuators -- $ADOPTED -D warnings && cargo build --target x86_64-unknown-none --features boot-actuators && objdump -d --no-show-raw-insn -M intel target/x86_64-unknown-none/debug/kernel | grep -A32 -E "<(.*spurious_entry|.*unclaimed_entry)[^>]*>:$"); }
lint > "$L/selftest-build-head.log" 2>&1
echo "head: EXIT=$?"
for p in "$M"/selftests-*.patch; do
  name=$(basename "$p" .patch)
  git apply --check "$p" || { echo "$name: does not apply"; exit 9; }
  git apply "$p"
  lint > "$L/selftest-build-$name.log" 2>&1
  echo "$name: EXIT=$?"
  git apply -R "$p"
done
echo "status after restore: [$(git status --porcelain --ignore-submodules=none)]"
