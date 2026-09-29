#!/bin/bash
# The TCG model's half of S0, on the development host and never on the T14: `tcg.sh <kit> <out>`
# boots capture.sh's <kit> under the track's QEMU command line and writes what src/lib.rs reads.
set -euo pipefail
mkdir "$2"
qemu-system-x86_64 --version > "$2"/qemu-version.txt
qemu-system-x86_64 -nodefaults -machine q35 -cpu qemu64,+rdrand,+smap,+fsgsbase,+x2apic,+smep \
  -smp 2 -m 4G -kernel "$1"/vmlinuz-6.8.0-142-generic -initrd "$1"/s0.cpio \
  -append "console=ttyS0 panic=-1" -serial stdio -display none -no-reboot < /dev/null > "$2"/console.txt
