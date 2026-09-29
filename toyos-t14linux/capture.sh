#!/bin/bash
# S0 of issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md: on the T14 under
# its stock Ubuntu, through sudo, `capture.sh <out> <kit>`. <out> is the evidence src/lib.rs
# reads; <kit> is the kernel and initramfs tcg.sh boots, never committed. It installs nothing,
# asks no network, and writes nothing but the two directories it creates.
set -euo pipefail
trap 'echo "capture.sh:$LINENO: failed" >&2' ERR
k=6.8.0-142-generic
refuse() { echo "capture.sh: refused: $1" >&2; exit 1; }
: "${SUDO_UID:?run capture.sh through sudo}"
case $(cat /proc/version) in "Linux version $k "*) ;; *) refuse "the running kernel is not $k";; esac
[ "$(dpkg-query -W -f '${Version} ' linux-image-$k linux-modules-$k)" = "6.8.0-142.142 6.8.0-142.142 " ] ||
  refuse "the packages are not 6.8.0-142.142"
echo "3b8533dd9d235ca634ac58f82c5ce1ee35f12ef620693e17033184d2c9ca5890  /boot/config-$k" |
  sha256sum -c --status || refuse "/boot/config-$k is not the pinned config"
modprobe -a cpuid msr
mkdir "$1" "$2"
out=$(cd "$1" && pwd) kit=$(cd "$2" && pwd)
cd "$out"

cat /proc/version > version.txt
uname -a > uname.txt
cat /proc/cmdline > cmdline.txt
dpkg-query -W linux-image-$k linux-modules-$k > packages.txt
sha256sum /boot/config-$k > config-sha256.txt
# The track's hardening table and the CPU_MITIGATIONS menu, with the config's line numbers.
opts='RANDOMIZE_BASE|RANDOMIZE_MEMORY|ARCH_MMAP_RND_BITS|STACKPROTECTOR_STRONG|SLS|X86_USER_SHADOW_STACK'
opts+='|RESET_ATTACK_MITIGATION|RANDOMIZE_KSTACK_OFFSET_DEFAULT|ZERO_CALL_USED_REGS|VMAP_STACK'
opts+='|STRICT_KERNEL_RWX|DEBUG_WX|SLAB_FREELIST_RANDOM|SLAB_FREELIST_HARDENED|RANDOM_KMALLOC_CACHES'
opts+='|X86_INTEL_TSX_MODE_OFF|INTEL_IOMMU_DEFAULT_ON|INIT_ON_ALLOC_DEFAULT_ON|SCHED_STACK_END_CHECK'
opts+='|HARDENED_USERCOPY|X86_UMIP|INIT_STACK_ALL_ZERO|FORTIFY_SOURCE|UBSAN_(BOUNDS|SHIFT|BOOL|ENUM)'
opts+='|STRICT_MODULE_RWX|LEGACY_VSYSCALL_XONLY|SHUFFLE_PAGE_ALLOCATOR|BPF_JIT_ALWAYS_ON|MODULE_SIG'
opts+='|KEXEC_SIG|CPU_MITIGATIONS|MITIGATION_[A-Z0-9_]+'
grep -nE "^(# )?CONFIG_($opts)(=| is not set)" /boot/config-$k > config-hardening.txt
sysctl vm.mmap_rnd_bits > mmap_rnd_bits.txt
grep . /sys/devices/system/cpu/vulnerabilities/* > vulnerabilities.txt
cat /proc/cpuinfo > cpuinfo.txt
# dmesg's lines, from the journal's copy of this boot so a wrapped ring loses none.
journalctl -k -b 0 -o cat | grep -iE 'mitigat|vulnerab|not affected|spectre|microcode|x86/bugs|tsx' \
  > kernel-log.txt
shopt -s nullglob
grep . /sys/class/dmi/id/{bios_*,ec_*,sys_vendor,product_name,product_version} > dmi.txt

# CPU 0's leaves as `leaf subleaf eax ebx ecx edx`; the device reads subleaf:leaf at its offset.
cpuid() { dd if=/dev/cpu/0/cpuid bs=16 count=1 skip=$(($1 | $2 << 32)) iflag=skip_bytes status=none | od -An -tx4; }
for l in 0:0 1:0 6:0 7:0 7:1 7:2 0xd:0 0xd:1 0x14:0 0x80000000:0 0x80000008:0 0x80000021:0; do
  r=$(cpuid ${l%:*} ${l#*:})
  printf '%08x %08x%s\n' ${l%:*} ${l#*:} "$r"
done > cpuid.txt

# Every CPU's MSRs as `msr cpu value`. 0x122 exists where ARCH_CAPABILITIES bit 7
# (TSX_CTRL_MSR) is set, 0x10F where CPUID.(7,0):EDX bits 11 and 13 both are.
rdmsr() { dd if=/dev/cpu/$1/msr bs=8 count=1 skip=$(($2)) iflag=skip_bytes status=none | od -An -tx8 | tr -d ' '; }
msrs=(0x8b 0x48 0x10a 0x123)
arch=$(rdmsr 0 0x10a)
r=$(cpuid 7 0)
read -r _ _ _ edx <<< "$r"
(( 0x$arch >> 7 & 1 )) && msrs+=(0x122)
(( 0x$edx >> 11 & 1 && 0x$edx >> 13 & 1 )) && msrs+=(0x10f)
for m in "${msrs[@]}"; do
  for c in /dev/cpu/[0-9]*; do
    v=$(rdmsr ${c##*/} $m)
    printf '%08x %s %s\n' $m ${c##*/} $v
  done
done > msr.txt

# The TCG model's kit: the track's initramfs around this machine's busybox and, where
# it is dynamic, what it links.
cp /boot/vmlinuz-$k "$kit"
bb=$(command -v busybox)
mkdir -p "$kit"/rd/bin "$kit"/rd/sys "$kit"/rd/proc
cp "$bb" "$kit"/rd/bin/busybox
if ldd "$bb" > /dev/null 2>&1; then cp --parents $(ldd "$bb" | grep -o '/[^ ]*') "$kit"/rd; fi
printf '#!/bin/busybox sh\n/bin/busybox mount -t sysfs s /sys\n/bin/busybox mount -t proc p /proc\n/bin/busybox cat /proc/version\n/bin/busybox grep . /sys/devices/system/cpu/vulnerabilities/*\n/bin/busybox poweroff -f\n' > "$kit"/rd/init
chmod +x "$kit"/rd/init
(cd "$kit"/rd && find . | cpio -o -H newc --quiet) > "$kit"/s0.cpio
chown -R "$SUDO_UID:$SUDO_GID" "$out" "$kit"
echo "capture.sh: wrote $out and $kit"
