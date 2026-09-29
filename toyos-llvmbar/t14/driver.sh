#!/bin/bash
# As the build's user, with sampler.sh running on the same run directory, which holds
# block.txt: s1, s2, one complete s3 line, then the stage-3 spans s3-1 to s3-3 back to
# back. Every configure and ninja line is block.txt's own line, eval'd.
set -eu
R=$1; B=$R/block.txt
line() {
  local l n
  l=$(awk -v p="$1" 'index($0, p) == 1' $B); n=$(printf '%s\n' "$l" | grep -c .)
  [ "$n" = 1 ] || { echo "block: $n lines start with '$1'" >&2; exit 1; }
  printf '%s\n' "$l"
}
L_W=$(line 'W=$HOME/llvm-baseline;'); L_CONF=$(line 'CONF='); L_LIBCXX=$(line 'LIBCXX=')
L_C1=$(line 'rm -rf s1;'); L_B1=$(line '/usr/bin/time')
L_C2=$(line 'rm -rf s2;'); L_B2=$(line 'PATH=$W/s1/bin:$PATH /usr/bin/time')
L_C3=$(line 'rm -rf s3;'); L_B3=$(line 'PATH=$W/s2/bin:$PATH /usr/bin/time')
eval "$L_W"; cd $W; eval "$L_CONF"; eval "$L_LIBCXX"
[ "$(git -C src rev-parse HEAD)" = "$SHA" ] || { echo "src is not at $SHA"; exit 1; }
[ -z "$(git -C src status --porcelain)" ] || { echo "src is not clean"; exit 1; }
SPID=$(cat $R/sampler.pid)
exec 3<>$R/phase.fifo
phase() {
  [ -d /proc/$SPID ] || { echo "sampler $SPID gone"; exit 1; }
  echo "$1" >&3
  echo "== $1 $(date -u +%FT%T.%3NZ)"
}
trap 'echo stop >&3' EXIT
# The kernel, command line, mitigations and microcode by S0's own commands
# (issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md), then the BIOS and the boot.
machine() {
  { cat /proc/version /proc/cmdline; grep . /sys/devices/system/cpu/vulnerabilities/*; grep microcode /proc/cpuinfo
    grep -H . /sys/class/dmi/id/bios_version /proc/sys/kernel/random/boot_id; } > $R/$1.txt
}
KEYS='^(CMAKE_BUILD_TYPE|CMAKE_C_COMPILER|CMAKE_CXX_COMPILER|CMAKE_C_FLAGS|CMAKE_CXX_FLAGS|CMAKE_C_FLAGS_RELEASE|CMAKE_CXX_FLAGS_RELEASE|CMAKE_(EXE|SHARED|MODULE|STATIC)_LINKER_FLAGS(_RELEASE)?|LLVM_ENABLE_LTO|LLVM_BUILD_INSTRUMENTED|LLVM_PROFDATA_FILE|LLVM_ENABLE_LLD|LLVM_ENABLE_PROJECTS|LLVM_ENABLE_RUNTIMES|LLVM_ENABLE_LIBCXX|LLVM_STATIC_LINK_CXX_STDLIB|LIBCXX_STATICALLY_LINK_ABI_IN_STATIC_LIBRARY|LLVM_TARGETS_TO_BUILD|LLVM_ENABLE_ASSERTIONS|CXX_(COMPILER|LINKER)_SUPPORTS_(STATIC_)?STDLIB)[:=]'
run() { # name stage configure-line ninja-line
  local name=$1 s=$2
  eval "$3"
  grep -E "$KEYS" $s/CMakeCache.txt > $R/$name-cache.txt
  grep -E "compiler identification|Linker detection|lld project|libc\+\+|WARNING" $s-config.log >> $R/$name-cache.txt || true
  mv $s-config.log $R/$name-config.log
  sync
  machine $name-machine-start
  phase $name-start
  eval "$4"
  phase $name-end
  machine $name-machine-end
  mv $s-time.txt $R/$name-time.txt; mv $s-build.log $R/$name-build.log
  echo "== $name done: $(grep -E 'Elapsed|Exit status' $R/$name-time.txt | tr '\n' ' ')"
}
run s1 s1 "$L_C1" "$L_B1"
run s2 s2 "$L_C2" "$L_B2"
for f in clang clang++ ld.lld; do [ -x s2/bin/$f ] || { echo "s2/bin/$f missing"; exit 1; }; done
for N in 0 1 2 3; do run s3-$N s3 "$L_C3" "$L_B3"; done
ninja -C s2 -t commands clang lld > $R/s2-commands.txt
ninja -C s3 -t commands clang lld > $R/s3-commands.txt
echo "== end $(date -u +%FT%T.%3NZ)"
