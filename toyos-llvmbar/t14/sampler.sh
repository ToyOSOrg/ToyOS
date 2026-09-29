#!/bin/bash
# As root, beside driver.sh: reads every envelope element into samples-<phase>.log at each
# phase the driver writes to phase.fifo, tagged read=start or read=end, and every 60 s
# between, tagged read=timed.
set -eu
R=$1
P=/sys/class/powercap
CPUS="0 1 2 3 4 5 6 7"
[ "$(ls -d /dev/cpu/[0-9]* | wc -l)" = 8 ] || { echo "expected 8 CPUs" >&2; exit 1; }
[ -e /dev/cpu/0/msr ] || { echo "no /dev/cpu/0/msr: modprobe msr" >&2; exit 1; }
T=
for h in /sys/class/hwmon/hwmon*; do [ "$(cat $h/name)" = coretemp ] && T=$h; done
[ "$(cat $T/temp1_label)" = "Package id 0" ] || { echo "no package temp" >&2; exit 1; }
mch=$(dd if=/sys/bus/pci/devices/0000:00:00.0/config bs=1 skip=$((0x48)) count=8 status=none | od -An -tx8 | tr -d ' ')
[ $((0x$mch & 1)) = 1 ] || { echo "MCHBAR disabled: $mch" >&2; exit 1; }
MMIO_PL=$(( (0x$mch & ~0x7fff) + 0x59a0 ))
rd() { dd if=/dev/cpu/$1/msr bs=8 count=1 skip=$(($2)) iflag=skip_bytes status=none | od -An -tx8 | tr -d ' '; }
percpu() { local v= c; for c in $CPUS; do v=$v${v:+,}$(rd $c $1); done; echo $v; }
b38() { local v= c x; for c in $CPUS; do x=$(rd $c 0x1a0); v=$v${v:+,}$(( (0x$x >> 38) & 1 )); done; echo $v; }
sysall() { local v= c; for c in $CPUS; do v=$v${v:+,}$(cat /sys/devices/system/cpu/cpu$c/cpufreq/$1); done; echo $v; }
rapl() { echo $(cat $P/$1/constraint_{0,1,2}_power_limit_uw) $(cat $P/$1/constraint_{0,1}_time_window_us) | tr ' ' ,; }
disks() { awk '$3 ~ /^(nvme[0-9]+n[0-9]+|sd[a-z]+)$/ {printf "%s%s:%s", s, $3, $6; s=","}' /proc/diskstats; }
[ -p $R/phase.fifo ] || mkfifo $R/phase.fifo
echo $$ > $R/sampler.pid
exec 3<>$R/phase.fifo
phase=idle; out=$R/samples-idle.log
sample() {
  echo "utc=$(date -u +%FT%T.%3NZ) epoch=$(date +%s) phase=$phase read=$1 temp_mc=$(cat $T/temp1_input) disk_rd_sectors=$(disks)" \
    "ac=$(cat /sys/class/power_supply/AC/online) platform_profile=$(cat /sys/firmware/acpi/platform_profile)" \
    "rapl_msr=$(rapl intel-rapl:0) rapl_mmio=$(rapl intel-rapl-mmio:0)" \
    "msr_610=$(rd 0 0x610) mmio_59a0=$(dd if=/dev/mem bs=8 count=1 skip=$MMIO_PL iflag=skip_bytes status=none | od -An -tx8 | tr -d ' ') msr_601=$(rd 0 0x601)" \
    "msr_770=$(percpu 0x770) msr_774=$(percpu 0x774) msr_1b0=$(percpu 0x1b0) msr_1a0_b38=$(b38) msr_772=$(rd 0 0x772) msr_8b=$(percpu 0x8b)" \
    "governor=$(sysall scaling_governor) epp=$(sysall energy_performance_preference) min_khz=$(sysall scaling_min_freq) max_khz=$(sysall scaling_max_freq)" \
    "pstate=$(cat /sys/devices/system/cpu/intel_pstate/status) no_turbo=$(cat /sys/devices/system/cpu/intel_pstate/no_turbo) hwp_dynamic_boost=$(cat /sys/devices/system/cpu/intel_pstate/hwp_dynamic_boost)" \
    "energy_uj=$(cat $P/intel-rapl:0/energy_uj)" >> "$out"
}
sample timed; next=$(($(date +%s) + 60))
while :; do
  wait_s=$((next - $(date +%s))); [ $wait_s -ge 1 ] || wait_s=1
  if read -t $wait_s -u 3 line; then
    case $line in
      *-start) phase=${line%-start}; out=$R/samples-$phase.log; sample start ;;
      *-end) sample end; phase=idle; out=$R/samples-idle.log ;;
      stop) sample timed; exit 0 ;;
      *) echo "unknown phase: $line" >&2; exit 1 ;;
    esac
  else
    rc=$?; [ $rc -gt 128 ] || { echo "fifo read failed: $rc" >&2; exit 1; }
    sample timed; next=$(($(date +%s) + 60))
  fi
done
