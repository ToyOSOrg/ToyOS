#!/bin/sh
# What this job is about to measure with, named before it measures anything.
# Three variables a verdict from this job must be read against: the QEMU
# version against `.github/qemu-version` — a disagreement reds, since the
# image and the archive under it are one date and this is the assertion that
# the pair still delivers the declared instrument; the host CPU vendor, since
# `kvm_amd` and
# `kvm_intel` are both in play
# and not selectable; and whether `/dev/kvm` is there *and opens*, the only
# difference between a `guest` shard and the `tcg` canary.
#
# Run from the repository root, after the checkout, by every job that boots a
# guest.
set -eu

here=$(dirname "$0")

want=$(grep -v '^#' "$here/qemu-version" | tr -d '[:space:]')
first=$(qemu-system-x86_64 --version | head -1)
have=$(echo "$first" | sed -n '1s/^QEMU emulator version \([^ ]*\).*/\1/p')

echo "$first"
if [ -f /dev/kvm ] || [ -c /dev/kvm ]; then
  accel=$(ls -l /dev/kvm)
  node=yes
else
  accel="no /dev/kvm node: this is the emulated arm"
  node=no
fi
echo "$accel"
cpu=""
[ -r /proc/cpuinfo ] && cpu=$(sed -n 's/^model name[^:]*: //p' /proc/cpuinfo | head -1)
cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo '?')
echo "cpu: ${cpu:-unknown}, $cores core(s)"

if [ "$have" != "$want" ]; then
  echo "::error::this job runs QEMU '${have:-$first}' and .github/qemu-version declares $want."
  echo "::error::The QEMU version decides test outcomes: 8.2.2 and 11.0.3 were measured"
  echo "::error::disagreeing about the same tree, so a number taken on one is not a number"
  echo "::error::about the other, and the dev host's baseline is recorded on $want."
  echo "::error::The image and the archive under it are one date (.github/apt-snapshot), so"
  echo "::error::this is about the tree: that date moved and .github/qemu-version did not,"
  echo "::error::or a QEMU from outside the pin is on PATH. The remedy for the first is one"
  echo "::error::line, in a commit that says the instrument changed."
  exit 1
fi

# Presence is not permission. `src/lib.rs`'s `kvm_usable` opens the node with
# O_RDWR and every boot follows its answer, so a job that *has* `/dev/kvm` and
# cannot open it emulates the whole suite and reports numbers taken on another
# instrument — the same class of silent drift the version check above refuses,
# and nothing else here would see it. The `tcg` canary has no node at all and
# is untouched.
if [ "$node" = yes ] && ! (exec 3<>/dev/kvm) 2>/dev/null; then
  echo "::error::/dev/kvm is here and this job cannot open it, so every boot would fall"
  echo "::error::back to emulation while the log above says the accelerator is present."
  echo "::error::Nothing about the tree: the job's user is outside the group that owns"
  echo "::error::the node."
  exit 1
fi

echo "- \`${GITHUB_JOB:-job}\` ${MATRIX_LABEL:-}: QEMU $have, ${cpu:-unknown CPU}, $cores core(s)" \
  >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
