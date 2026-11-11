#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# The host half of a vtrace.py capture, which runs it through sudo (tracefs and KVM's debugfs need root):
#   sudo hosttrace.sh SECONDS OUT_DIR QEMU_PID
# Records, on the raw TSC clock (the clock the guest's trace uses, offset by KVM), for SECONDS:
#   - the GPU scheduler's jobs (queued, run, done) and amdgpu's command submissions: when the host GPU works, for whom
#   - when the threads of QEMU and its render servers (vCPUs, Venus' vkr-ring/vkr-queue threads) run
# into OUT_DIR/host.txt (ftrace's text), with KVM's guest TSC offset and scaling in kvm.txt and the host's threads in
# threads.txt (pid tid comm). Touches OUT_DIR/host.ready once recording, for vtrace.py to start the guest. Restores
# tracing to how it found it. Files end up owned by the invoking user.
set -euo pipefail
secs=$1 out=$2 qemu=$3
t=/sys/kernel/tracing
d=/sys/kernel/debug

[ -d "$t/events" ] || mount -t tracefs nodev "$t"
[ -d "$d/kvm" ] || mount -t debugfs nodev "$d"

restore() {
    echo 0 > "$t/tracing_on"
    echo 0 > "$t/events/enable"
    echo 0 > "$t/events/sched/sched_switch/filter"
    echo local > "$t/trace_clock"
    echo 1408 > "$t/buffer_size_kb"
    chown -R "${SUDO_UID:-0}:${SUDO_GID:-0}" "$out"
}
trap restore EXIT

for f in "$d"/kvm/"$qemu"-*/vcpu*/tsc-offset "$d"/kvm/"$qemu"-*/vcpu*/tsc-scaling-ratio; do
    [ -e "$f" ] && echo "$f $(cat "$f")"
done > "$out/kvm.txt"
[ -s "$out/kvm.txt" ] || { echo "hosttrace: no KVM vCPUs for pid $qemu" >&2; exit 1; }
ps -eLo pid=,tid=,comm= > "$out/threads.txt"

echo 0 > "$t/tracing_on"
echo > "$t/trace"
echo x86-tsc > "$t/trace_clock"
echo 16384 > "$t/buffer_size_kb"
echo 0 > "$t/events/enable"
echo 1 > "$t/events/gpu_scheduler/enable"
for e in amdgpu_cs_ioctl amdgpu_sched_run_job; do
    [ -e "$t/events/amdgpu/$e/enable" ] && echo 1 > "$t/events/amdgpu/$e/enable"
done
# QEMU's threads (the Nix wrapper names the main one .qemu-system-x8), its vCPUs and the render servers'.
filter=
for c in '*qemu*' 'CPU *' 'virgl*' 'vkr-*' 'vrend*'; do
    filter="$filter${filter:+ || }prev_comm ~ \"$c\" || next_comm ~ \"$c\""
done
echo "$filter" > "$t/events/sched/sched_switch/filter"
echo 1 > "$t/events/sched/sched_switch/enable"

echo 1 > "$t/tracing_on"
touch "$out/host.ready"
timeout "$secs" cat "$t/trace_pipe" > "$out/host.txt" || true
echo "hosttrace: $(wc -l < "$out/host.txt") events"
