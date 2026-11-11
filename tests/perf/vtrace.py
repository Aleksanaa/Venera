#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Performance traces across the guest and the host, on one clock.

  vtrace.py capture SECONDS RUN_DIR [--qga SOCK] [--qemu-pid PID] [--no-host]   (in nix develop: Python)
      Records SECONDS in the guest (tests/win/venera-trace.exe, run through the QEMU guest agent: our UMDs' and Venus
      ICD's events, dxgkrnl's and DWM's) and on the host (hosttrace.sh through sudo: the GPU scheduler's jobs, when
      QEMU's and the render servers' threads run), then writes the Perfetto timeline and prints the summary.
  vtrace.py perfetto RUN_DIR
      RUN_DIR/trace.json: open it in https://ui.perfetto.dev.
  vtrace.py summary RUN_DIR [RUN_DIR...]
      Latency and throughput through the pipeline, runs side by side; per uibench scenario for venera-test.ps1 Perf
      runs (uibench.txt).

A run directory holds events.jsonl (the guest's trace, decoded by venera-trace) and, from a capture, host.txt,
kvm.txt and threads.txt (hosttrace.sh). Guest timestamps are the guest's TSC, which KVM keeps at the host's plus an
offset (kvm.txt): host = guest - offset.
"""
import argparse
import base64
import bisect
import json
import os
import re
import socket
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
GUEST_DIR = r'C:\venera'

# A D3D12 batch through the queue bridge (umd/d3d12/queue_bridge.cpp): instants of one queue (id) and batch (v), in
# order, and the stages between them. Until its gate opens a batch waits behind the queue's earlier work: how far the
# application runs ahead, not a latency.
D3D12_STAGES = ['d3d12.queued', 'd3d12.open', 'd3d12.finished', 'd3d12.done']
D3D12_STAGE_NAMES = ['queued ahead', 'gpu', 'signal']

# Venus' submissions to the host (spans of our ICD), for when the guest handed the host GPU work.
VENUS_SUBMITS = {'vn_QueueSubmit', 'vn_QueueSubmit2'}

# The host threads hosttrace.sh records the scheduling of: QEMU's, its vCPUs and the render servers'.
VM_THREAD = re.compile(r'qemu|^CPU |^virgl|^vkr-|^vrend')

# Perfetto process ids of what is not a guest process.
HOST_PID = 1_000_000
HOST_GPU_PID = 2_000_000
GUEST_DISPLAY_PID = 3_000_000


# --- Loading ---------------------------------------------------------------------------------------------------------

class Run:
    def __init__(self, path):
        self.dir = Path(path)
        self.name = self.dir.name
        self.meta = {}
        self.procs, self.threads = {}, {}
        self.spans = []      # (start, end, pid, tid, name, value), guest TSC
        self.instants = []   # (t, pid, tid, name, id, value)
        self.counters = []   # (t, pid, tid, name, value)
        self.etw = []        # (t, pid, tid, provider, id, ev, fields)
        self._load_guest()
        self.offset = 0
        self.host = None
        if (self.dir / 'host.txt').exists():
            self.offset = load_kvm_offset(self.dir / 'kvm.txt')
            self.host = Host(self.dir)
        syncs = [i for i in self.instants if i[3] == 'venera-trace.sync']
        self.sync_error_us = max((abs(i[0] - i[5]) / self.hz * 1e6 for i in syncs), default=None)
        times = [s[0] for s in self.spans] + [i[0] for i in self.instants] + [e[0] for e in self.etw]
        if len(syncs) >= 2:
            self.window = (syncs[0][0], syncs[-1][0])
        elif times:
            self.window = (min(times), max(times))
        else:
            sys.exit(f'{self.dir}: no events')

    def _load_guest(self):
        stacks = defaultdict(list)
        with open(self.dir / 'events.jsonl', encoding='utf-8') as f:
            for line in f:
                e = json.loads(line)
                k = e['k']
                if k == 'begin':
                    stacks[e['tid']].append((e['t'], e['name'], e['v']))
                elif k == 'end':
                    if stacks[e['tid']]:
                        t, name, v = stacks[e['tid']].pop()
                        self.spans.append((t, e['t'], e['pid'], e['tid'], name, v))
                elif k == 'instant':
                    self.instants.append((e['t'], e['pid'], e['tid'], e['name'], e['id'], e['v']))
                elif k == 'counter':
                    self.counters.append((e['t'], e['pid'], e['tid'], e['name'], e['v']))
                elif k == 'etw':
                    self.etw.append((e['t'], e['pid'], e['tid'], e['p'], e['id'], e['ev'], e['f']))
                elif k == 'proc':
                    self.procs[e['pid']] = e['name']
                elif k == 'thread':
                    self.threads[e['tid']] = e['name']
                elif k == 'meta':
                    self.meta = e
        self.hz = self.meta.get('tsc_hz') or sys.exit(f'{self.dir}: events.jsonl has no TSC rate')
        self.spans.sort()
        self.etw.sort(key=lambda e: e[0])

    def scenarios(self):
        """(name, start, end) in guest TSC: uibench's scenarios, else the whole trace."""
        out = []
        report = self.dir / 'uibench.txt'
        if report.exists():
            for line in report.read_text(encoding='utf-8', errors='replace').splitlines():
                if line.startswith('scenario '):
                    parts = line.split()
                    kv = dict(p.split('=', 1) for p in parts[2:])
                    out.append((parts[1], int(kv['start']), int(kv['end'])))
        return out or [('trace', *self.window)]

    def ms(self, ticks):
        return ticks / self.hz * 1e3


def load_kvm_offset(path):
    offsets, ratios = set(), set()
    for line in path.read_text().splitlines():
        name, value = line.rsplit(' ', 1)
        (offsets if name.endswith('tsc-offset') else ratios).add(int(value))
    if len(offsets) != 1:
        sys.exit(f'{path}: the vCPUs have different TSC offsets: {sorted(offsets)}')
    # 1.0 in AMD's (32 fraction bits) or Intel's (48) fixed point.
    if ratios - {1 << 32, 1 << 48}:
        sys.exit(f'{path}: the guest TSC is scaled ({sorted(ratios)}); vtrace.py assumes it runs at the host\'s rate')
    return offsets.pop()


FTRACE_LINE = re.compile(r'^\s*(.+?)-(\d+)\s+(?:\(\s*[-\d]+\)\s+)?\[(\d+)\]\s+\S+\s+(\d+):\s+(\w+):\s?(.*)$')
SWITCH = re.compile(r'prev_comm=(.*?) prev_pid=(\d+) .*?==> next_comm=(.*?) next_pid=(\d+)')
FENCE = re.compile(r'fence=([0-9a-fx]+:[0-9a-fx]+)')
RING = re.compile(r'\bring=([\w.]+)')


class Host:
    """hosttrace.sh's recording, in host TSC."""

    def __init__(self, path):
        self.thread_pid, self.comm = {}, {}
        for line in (path / 'threads.txt').read_text().splitlines():
            parts = line.split(None, 2)
            if len(parts) == 3:
                self.thread_pid[int(parts[1])] = int(parts[0])
                self.comm[int(parts[1])] = parts[2]
        self.running = defaultdict(list)   # tid: [(start, end)]
        self.jobs = {}                     # fence: {queue, run, done, ring, tid}
        self.submits = []                  # (t, tid, fence): amdgpu command submissions
        started = {}
        with open(path / 'host.txt', encoding='utf-8', errors='replace') as f:
            for line in f:
                m = FTRACE_LINE.match(line)
                if not m:
                    continue
                comm, tid, _, t, event, rest = m.groups()
                tid, t = int(tid), int(t)
                if event == 'sched_switch':
                    s = SWITCH.search(rest)
                    if not s:
                        continue
                    # An event passes hosttrace.sh's filter when either thread is the VM's: only those are kept.
                    prev, nxt = int(s.group(2)), int(s.group(4))
                    if prev in started:
                        self.running[prev].append((started.pop(prev), t))
                    if VM_THREAD.search(s.group(3)):
                        self.comm.setdefault(nxt, s.group(3))
                        started[nxt] = t
                    continue
                fm = FENCE.search(rest)
                if not fm:
                    continue
                job = self.jobs.setdefault(fm.group(1), {})
                if event == 'amdgpu_cs_ioctl':
                    self.submits.append((t, tid, fm.group(1)))
                    self.comm.setdefault(tid, comm)
                    job['tid'] = tid
                elif event == 'drm_sched_job_queue':
                    job['queue'] = t
                    rm = RING.search(rest)
                    if rm:
                        job['ring'] = rm.group(1)
                elif event == 'drm_sched_job_run':
                    job['run'] = t
                    rm = RING.search(rest)
                    if rm:
                        job['ring'] = rm.group(1)
                elif event == 'drm_sched_job_done':
                    job['done'] = t
        self.jobs = {f: j for f, j in self.jobs.items() if 'run' in j and 'done' in j and j['done'] >= j['run']}

    def submitter(self, job):
        tid = job.get('tid')
        return self.comm.get(tid, '?') if tid is not None else '?'


def group_name(comm):
    """Threads by role: digits dropped (vkr-ring-24 and vkr-ring-8 are both vkr-ring-#)."""
    return re.sub(r'\d+', '#', comm)


# --- Summary ---------------------------------------------------------------------------------------------------------

def pct(values, p):
    if not values:
        return float('nan')
    v = sorted(values)
    return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))]


def union_length(intervals, a, b):
    total, end = 0, a
    for s, e in sorted(intervals):
        s, e = max(s, end), min(e, b)
        if e > s:
            total += e - s
            end = e
    return total


def summarize(run, a, b):
    """{section: [(label, value)]} for the guest TSC window [a, b]."""
    secs = (b - a) / run.hz
    out = {}

    rows = []
    if run.sync_error_us is not None:
        rows.append(('clock check: |etw t - rdtsc| us', f'{run.sync_error_us:.1f}'))
    rows.append(('events lost', str(run.meta.get('events_lost', 0))))
    rows.append(('seconds', f'{secs:.2f}'))
    out['trace'] = rows

    # Our spans, by total time.
    by_name = defaultdict(list)
    for s, e, *_, name, _v in run.spans:
        if a <= s <= b:
            by_name[name].append(e - s)
    rows = []
    for name, durs in sorted(by_name.items(), key=lambda kv: -sum(kv[1]))[:24]:
        ms = [run.ms(d) for d in durs]
        rows.append((f'{name}: /s', f'{len(ms) / secs:.1f}'))
        rows.append((f'{name}: p50 | p99 ms', f'{pct(ms, 50):.3f} | {pct(ms, 99):.3f}'))
        rows.append((f'{name}: time %', f'{100 * sum(ms) / 1e3 / secs:.1f}'))
    out['guest spans (UMDs, Venus)'] = rows

    # D3D12 batches through the queue bridge.
    batches = defaultdict(dict)
    for t, pid, _, name, ident, v in run.instants:
        if name in D3D12_STAGES:
            batches[(pid, ident, v)][name] = t
    stages = defaultdict(list)
    count = 0
    complete = {k: st for k, st in batches.items() if all(n in st for n in D3D12_STAGES)}
    # The host GPU's last job of the VM to finish before the guest noticed a batch finished.
    vm_done = sorted(j['done'] + run.offset for j in run.host.jobs.values()
                     if run.host.submitter(j).startswith('vkr')) if run.host else []
    for (pid, queue, v), st in complete.items():
        if not a <= st[D3D12_STAGES[0]] <= b:
            continue
        count += 1
        for i, label in enumerate(D3D12_STAGE_NAMES):
            stages[label].append(run.ms(st[D3D12_STAGES[i + 1]] - st[D3D12_STAGES[i]]))
        stages['total'].append(run.ms(st[D3D12_STAGES[-1]] - st[D3D12_STAGES[0]]))
        # A batch queued before the previous one finished opens only once that one's done is through the kernel:
        # the GPU waits for this round trip between the two.
        prev = complete.get((pid, queue, v - 1))
        if prev and st['d3d12.queued'] < prev['d3d12.finished']:
            stages['previous done > open'].append(run.ms(st['d3d12.open'] - prev['d3d12.done']))
        i = bisect.bisect_right(vm_done, st['d3d12.finished']) - 1
        if i >= 0:
            stages['host GPU done > finished'].append(run.ms(st['d3d12.finished'] - vm_done[i]))
    if count:
        rows = [('batches /s', f'{count / secs:.1f}')]
        for label in D3D12_STAGE_NAMES + ['total', 'host GPU done > finished', 'previous done > open']:
            if stages[label]:
                rows.append((f'{label} p50 | p90 | p99 ms',
                             ' | '.join(f'{pct(stages[label], p):.3f}' for p in (50, 90, 99))))
        out['D3D12 batches (queued > open > finished > done)'] = rows

    if run.host:
        out.update(summarize_host(run, a, b, secs))
    out.update(summarize_display(run, a, b, secs))
    return out


def summarize_host(run, a, b, secs):
    host = run.host
    ha, hb = a - run.offset, b - run.offset
    out = {}
    rows = []
    rings = defaultdict(list)
    for job in host.jobs.values():
        rings[job.get('ring', '?')].append(job)
    for ring, jobs in sorted(rings.items()):
        inside = [j for j in jobs if j['done'] >= ha and j['run'] <= hb]
        if not inside:
            continue
        busy = union_length([(j['run'], j['done']) for j in inside], ha, hb) / (hb - ha)
        rows.append((f'{ring}: busy %', f'{100 * busy:.1f}'))
        rows.append((f'{ring}: jobs /s', f'{len(inside) / secs:.0f}'))
        by_submitter = defaultdict(list)
        for j in inside:
            by_submitter[group_name(host.submitter(j))].append((j['run'], j['done']))
        for who, iv in sorted(by_submitter.items(), key=lambda kv: -union_length(kv[1], ha, hb))[:4]:
            rows.append((f'{ring}: busy % for {who}', f'{100 * union_length(iv, ha, hb) / (hb - ha):.1f}'))
        queued = [run.ms(j['run'] - j['queue']) for j in inside if 'queue' in j]
        if queued:
            rows.append((f'{ring}: queue>run p50 | p99 ms', f'{pct(queued, 50):.3f} | {pct(queued, 99):.3f}'))
        run_ms = [run.ms(j['done'] - j['run']) for j in inside]
        rows.append((f'{ring}: run>done p50 | p99 ms', f'{pct(run_ms, 50):.3f} | {pct(run_ms, 99):.3f}'))
        rows += idle_split(run, ring, inside, ha, hb)
    out['host GPU'] = rows

    groups = defaultdict(int)
    for tid, iv in host.running.items():
        groups[group_name(host.comm.get(tid, str(tid)))] += union_length(iv, ha, hb)
    rows = [(f'{g}: CPU %', f'{100 * v / (hb - ha):.1f}')
            for g, v in sorted(groups.items(), key=lambda kv: -kv[1])[:16]]
    out['host threads (100% = one CPU)'] = rows
    return out


def idle_split(run, ring, jobs, ha, hb):
    """Why the ring idled, as % of the window: until each gap's next job of the VM, the guest had not submitted it
    yet, it was on its way to the host (guest submit to host submission), or the host scheduler held it; or the next
    job was another client's."""
    host = run.host
    submits = sorted(e - run.offset for s, e, *_, name, _v in run.spans if name in VENUS_SUBMITS)
    if not submits or not any(host.submitter(j).startswith('vkr') for j in jobs):
        return []
    split = defaultdict(int)
    jobs = sorted(jobs, key=lambda j: j['run'])
    end = jobs[0]['done']
    for j in jobs[1:]:
        s, e = max(end, ha), min(j['run'], hb)
        end = max(end, j['done'])
        if e <= s:
            continue
        if not host.submitter(j).startswith('vkr'):
            split['another client\'s job next'] += e - s
            continue
        cs = min(max(j.get('queue', e), s), e)
        i = bisect.bisect_right(submits, j.get('queue', e)) - 1
        guest = min(max(submits[i] if i >= 0 else s, s), cs)
        split['guest had not submitted'] += guest - s
        split['guest submit > host submission'] += cs - guest
        split['host scheduler'] += e - cs
    total = hb - ha
    rows = [(f'{ring}: idle %', f'{100 * sum(split.values()) / total:.1f}')]
    rows += [(f'{ring}: idle % {k}', f'{100 * v / total:.1f}') for k, v in split.items()]
    return rows


DXGK, DWM = 'DxgKrnl', 'Dwm'


def summarize_display(run, a, b, secs):
    dwm_pids = {pid for pid, name in run.procs.items() if name.lower() == 'dwm.exe'}
    inside = [e for e in run.etw if a <= e[0] <= b]
    if not inside:
        return {}

    def times(provider, ident):
        return [e[0] for e in inside if e[3] == provider and e[4] == ident]

    def intervals(ts):
        return [run.ms(y - x) for x, y in zip(ts, ts[1:])]

    vsync, flips, frames = times(DXGK, 17), times(DXGK, 116), times(DWM, 10)
    vi, fl, fr = intervals(vsync), intervals(flips), intervals(frames)
    period = pct(vi, 50)
    frame_cpu, open_frame = [], None
    for t, _, _, p, i, _, _ in inside:
        if p == DWM and i == 10:
            open_frame = t
        elif p == DWM and i == 12 and open_frame is not None:
            frame_cpu.append(run.ms(t - open_frame))
            open_frame = None
    to_flip, fi = [], 0
    for t, pid, _, p, i, _, _ in inside:
        if p == DXGK and i == 184 and pid in dwm_pids:
            while fi < len(flips) and flips[fi] < t:
                fi += 1
            if fi < len(flips):
                to_flip.append(run.ms(flips[fi] - t))
    # GPU packets: queued (QueuePacket start) and started (DmaPacket start) to completed (DmaPacket info, the
    # completion interrupt), by context and submit sequence. The whole trace, so packets crossing the edges match.
    queued, started, latency = {}, {}, []
    for t, pid, _, p, i, _, f in run.etw:
        if p != DXGK:
            continue
        if i == 178 and 'SubmitSequence' in f:
            queued[(f.get('hContext'), f['SubmitSequence'])] = (t, pid)
        elif i == 175 and 'ulQueueSubmitSequence' in f:
            started[(f.get('hContext'), f['ulQueueSubmitSequence'])] = t
        elif i == 177 and 'COMPLETED' in f.get('InterruptType', ''):
            k = (f.get('hContext'), f.get('ulQueueSubmitSequence'))
            if k in queued:
                tq, qpid = queued.pop(k)
                if a <= t <= b:
                    latency.append((qpid in dwm_pids, run.ms(t - tq), run.ms(t - started.pop(k, tq))))
    f2 = lambda v: f'{v:.2f}'
    rows = [
        ('VSync /s', f'{len(vsync) / secs:.1f}'),
        ('VSync interval p50 | p99 ms', f'{f2(period)} | {f2(pct(vi, 99))}'),
        ('DWM frames /s', f'{len(frames) / secs:.1f}'),
        ('DWM frame gap p50 | p99 ms', f'{f2(pct(fr, 50))} | {f2(pct(fr, 99))}'),
        ('DWM frame CPU p50 | p99 ms', f'{f2(pct(frame_cpu, 50))} | {f2(pct(frame_cpu, 99))}'),
        ('flips /s', f'{len(flips) / secs:.1f}'),
        ('flip gap p95 | p99 ms', f'{f2(pct(fl, 95))} | {f2(pct(fl, 99))}'),
        ('flip gaps > 2.5 periods', str(sum(1 for v in fl if v > 2.5 * period)) if period == period else '-'),
        ('DWM present>flip p50 | p99 ms', f'{f2(pct(to_flip, 50))} | {f2(pct(to_flip, 99))}'),
        ('GPU packets', str(len(latency))),
        ('DWM packet queue>done p50 | p99 ms',
         f'{f2(pct([l[1] for l in latency if l[0]], 50))} | {f2(pct([l[1] for l in latency if l[0]], 99))}'),
        ('all packets queue>done p99 ms', f2(pct([l[1] for l in latency], 99))),
        ('all packets start>done p99 ms', f2(pct([l[2] for l in latency], 99))),
    ]
    return {'guest display (dxgkrnl, DWM)': rows}


def cmd_summary(args):
    runs = [Run(p) for p in args.runs]
    for r in runs:
        info = r.dir / 'info.txt'
        if info.exists():
            lines = info.read_text(encoding='utf-8-sig', errors='replace').splitlines()
            print(f'{r.name}: ' + '; '.join(l for l in lines if l.startswith(('driver', 'file venera', 'logical'))))
    scenarios = list(dict.fromkeys(name for r in runs for name, _, _ in r.scenarios()))
    width = max(18, *(len(r.name) + 2 for r in runs))
    for scenario in scenarios:
        tables = []
        for r in runs:
            window = next(((s, e) for n, s, e in r.scenarios() if n == scenario), None)
            tables.append(summarize(r, *window) if window else {})
        print(f'\n=== {scenario}')
        for section in dict.fromkeys(s for t in tables for s in t):
            labels = list(dict.fromkeys(label for t in tables for label, _ in t.get(section, [])))
            if not labels:
                continue
            print(f'\n-- {section}')
            label_width = max(len(label) for label in labels) + 2
            if len(runs) > 1:
                print(' ' * label_width + ''.join(f'{r.name:>{width}}' for r in runs))
            for label in labels:
                cells = [dict(t.get(section, [])).get(label, '-') for t in tables]
                print(f'{label:<{label_width}}' + ''.join(f'{c:>{width}}' for c in cells))


# --- Perfetto ---------------------------------------------------------------------------------------------------------

def cmd_perfetto(args):
    path = write_perfetto(Run(args.run))
    print(f'{path}: open it in https://ui.perfetto.dev')


def write_perfetto(run):
    """Chrome's JSON trace format, which Perfetto reads; host TSC, in microseconds from the trace's start."""
    base = run.window[0] - run.offset
    us = lambda host_tsc: round((host_tsc - base) / run.hz * 1e6, 3)
    g = lambda guest_tsc: us(guest_tsc - run.offset)
    events = []

    def name_process(pid, name):
        events.append({'ph': 'M', 'name': 'process_name', 'pid': pid, 'args': {'name': name}})

    def name_thread(pid, tid, name):
        events.append({'ph': 'M', 'name': 'thread_name', 'pid': pid, 'tid': tid, 'args': {'name': name}})

    guest_pids = {s[2] for s in run.spans} | {i[1] for i in run.instants} | {c[1] for c in run.counters} | \
                 {e[1] for e in run.etw}
    for pid in guest_pids:
        name_process(pid, f'guest {run.procs.get(pid, "?")} {pid}')
    thread_pid = {s[3]: s[2] for s in run.spans} | {i[2]: i[1] for i in run.instants}
    for tid, name in run.threads.items():
        if tid in thread_pid:
            name_thread(thread_pid[tid], tid, name)

    for s, e, pid, tid, name, v in run.spans:
        events.append({'ph': 'X', 'name': name, 'pid': pid, 'tid': tid, 'ts': g(s), 'dur': g(e) - g(s),
                       'args': {'v': v}})
    for t, pid, tid, name, v in run.counters:
        events.append({'ph': 'C', 'name': name, 'pid': pid, 'ts': g(t), 'args': {name: v}})

    batches = defaultdict(dict)
    for t, pid, tid, name, ident, v in run.instants:
        if name in D3D12_STAGES:
            batches[(pid, ident, v)][name] = t
        else:
            events.append({'ph': 'i', 's': 't', 'name': name, 'pid': pid, 'tid': tid, 'ts': g(t),
                           'args': {'id': ident, 'v': v}})
    for (pid, ident, v), st in batches.items():
        if not all(n in st for n in D3D12_STAGES):
            continue
        aid = f'{ident:x}:{v}'
        events.append({'ph': 'b', 'cat': 'd3d12 batch', 'name': f'batch {v}', 'id': aid, 'pid': pid,
                       'ts': g(st[D3D12_STAGES[0]])})
        for i, label in enumerate(D3D12_STAGE_NAMES):
            events.append({'ph': 'b', 'cat': 'd3d12 batch', 'name': label, 'id': aid, 'pid': pid,
                           'ts': g(st[D3D12_STAGES[i]])})
            events.append({'ph': 'e', 'cat': 'd3d12 batch', 'name': label, 'id': aid, 'pid': pid,
                           'ts': g(st[D3D12_STAGES[i + 1]])})
        events.append({'ph': 'e', 'cat': 'd3d12 batch', 'name': f'batch {v}', 'id': aid, 'pid': pid,
                       'ts': g(st[D3D12_STAGES[-1]])})

    # dxgkrnl and DWM: the display's beat on a track of its own, GPU packets on their process.
    name_process(GUEST_DISPLAY_PID, 'guest display')
    name_thread(GUEST_DISPLAY_PID, 1, 'VSync')
    name_thread(GUEST_DISPLAY_PID, 2, 'flips')
    name_thread(GUEST_DISPLAY_PID, 3, 'DWM frames')
    queued, frame = {}, None
    for t, pid, tid, p, i, ev, f in run.etw:
        if p == DXGK and i == 17:
            events.append({'ph': 'i', 's': 't', 'name': 'VSync', 'pid': GUEST_DISPLAY_PID, 'tid': 1, 'ts': g(t)})
        elif p == DXGK and i == 116:
            events.append({'ph': 'i', 's': 't', 'name': 'flip', 'pid': GUEST_DISPLAY_PID, 'tid': 2, 'ts': g(t)})
        elif p == DXGK and i == 184:
            events.append({'ph': 'i', 's': 't', 'name': 'dxgkrnl Present', 'pid': pid, 'tid': tid, 'ts': g(t)})
        elif p == DXGK and i == 178 and 'SubmitSequence' in f:
            queued[(f.get('hContext'), f['SubmitSequence'])] = (t, pid)
        elif p == DXGK and i == 177 and 'COMPLETED' in f.get('InterruptType', ''):
            k = (f.get('hContext'), f.get('ulQueueSubmitSequence'))
            if k in queued:
                tq, qpid = queued.pop(k)
                aid = f'{k[0]}:{k[1]}'
                events.append({'ph': 'b', 'cat': 'dxgkrnl packet', 'name': 'packet', 'id': aid, 'pid': qpid,
                               'ts': g(tq)})
                events.append({'ph': 'e', 'cat': 'dxgkrnl packet', 'name': 'packet', 'id': aid, 'pid': qpid,
                               'ts': g(t)})
        elif p == DWM and i == 10:
            frame = t
        elif p == DWM and i == 12 and frame is not None:
            events.append({'ph': 'X', 'name': 'DWM frame', 'pid': GUEST_DISPLAY_PID, 'tid': 3, 'ts': g(frame),
                           'dur': g(t) - g(frame)})
            frame = None

    if run.host:
        host = run.host
        for tid, iv in host.running.items():
            pid = HOST_PID + host.thread_pid.get(tid, tid)
            name_thread(pid, tid, host.comm.get(tid, str(tid)))
            for s, e in iv:
                events.append({'ph': 'X', 'name': 'running', 'pid': pid, 'tid': tid, 'ts': us(s),
                               'dur': us(e) - us(s)})
        for pid in {host.thread_pid.get(tid, tid) for tid in host.running}:
            name_process(HOST_PID + pid, f'host {host.comm.get(pid, "?")} {pid}')
        for t, tid, fence in host.submits:
            pid = HOST_PID + host.thread_pid.get(tid, tid)
            events.append({'ph': 'i', 's': 't', 'name': 'amdgpu CS', 'pid': pid, 'tid': tid, 'ts': us(t),
                           'args': {'fence': fence}})
        name_process(HOST_GPU_PID, 'host GPU')
        for fence, job in host.jobs.items():
            cat = job.get('ring', 'gpu')
            who = host.submitter(job)
            if 'queue' in job:
                events.append({'ph': 'b', 'cat': cat, 'name': f'{cat} queued', 'id': fence, 'pid': HOST_GPU_PID,
                               'ts': us(job['queue'])})
                events.append({'ph': 'e', 'cat': cat, 'name': f'{cat} queued', 'id': fence, 'pid': HOST_GPU_PID,
                               'ts': us(job['run'])})
            events.append({'ph': 'b', 'cat': cat, 'name': f'{cat} {who}', 'id': fence, 'pid': HOST_GPU_PID,
                           'ts': us(job['run']), 'args': {'fence': fence}})
            events.append({'ph': 'e', 'cat': cat, 'name': f'{cat} {who}', 'id': fence, 'pid': HOST_GPU_PID,
                           'ts': us(job['done'])})

    path = run.dir / 'trace.json'
    with open(path, 'w') as f:
        json.dump({'traceEvents': events, 'displayTimeUnit': 'ms'}, f, separators=(',', ':'))
    return path


# --- Capture ---------------------------------------------------------------------------------------------------------

class Agent:
    """The QEMU guest agent, over its socket (no libvirt, no password)."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.file = self.sock.makefile('rwb')
        token = int(time.time() * 1000) & 0x7fffffff
        self._send({'execute': 'guest-sync', 'arguments': {'id': token}})
        while self._receive().get('return') != token:   # drop replies a previous client left unread
            pass

    def _send(self, msg):
        self.file.write(json.dumps(msg).encode() + b'\n')
        self.file.flush()

    def _receive(self):
        line = self.file.readline()
        if not line:
            raise RuntimeError('the guest agent closed the connection')
        return json.loads(line)

    def call(self, command, **arguments):
        self._send({'execute': command, 'arguments': arguments} if arguments else {'execute': command})
        reply = self._receive()
        if 'error' in reply:
            raise RuntimeError(f'{command}: {reply["error"].get("desc")}')
        return reply['return']

    def run(self, path, args, timeout):
        pid = self.call('guest-exec', path=path, arg=args, **{'capture-output': True})['pid']
        end = time.time() + timeout
        while time.time() < end:
            st = self.call('guest-exec-status', pid=pid)
            if st.get('exited'):
                text = lambda k: base64.b64decode(st.get(k, '')).decode('utf-8', 'replace')
                return st.get('exitcode', -1), text('out-data') + text('err-data')
            time.sleep(0.5)
        raise TimeoutError(f'{path} still running after {timeout} s')

    def put(self, local, remote):
        h = self.call('guest-file-open', path=remote, mode='wb')
        try:
            data = Path(local).read_bytes()
            for i in range(0, len(data), 1 << 20):
                self.call('guest-file-write', handle=h, **{'buf-b64': base64.b64encode(data[i:i + (1 << 20)]).decode()})
        finally:
            self.call('guest-file-close', handle=h)

    def get(self, remote, local):
        h = self.call('guest-file-open', path=remote, mode='rb')
        try:
            with open(local, 'wb') as f:
                while True:
                    r = self.call('guest-file-read', handle=h, count=1 << 20)
                    f.write(base64.b64decode(r['buf-b64']))
                    if r['eof']:
                        break
        finally:
            self.call('guest-file-close', handle=h)


def find_qemu():
    pids = subprocess.run(['pgrep', '-f', 'qemu-system'], capture_output=True, text=True).stdout.split()
    if len(pids) != 1:
        sys.exit(f'vtrace: {len(pids)} QEMU processes running; name one with --qemu-pid')
    return int(pids[0])


def cmd_capture(args):
    if not args.qga:
        sys.exit('vtrace: name the guest agent socket: --qga or QGA_SOCK')
    out = Path(args.run)
    out.mkdir(parents=True, exist_ok=True)
    for name in ('host.ready', 'host.txt', 'kvm.txt', 'threads.txt', 'events.jsonl', 'trace.json'):
        (out / name).unlink(missing_ok=True)
    agent = Agent(args.qga)
    agent.run('cmd.exe', ['/c', 'mkdir', GUEST_DIR], 30)   # fails harmlessly if it exists
    exe = GUEST_DIR + r'\venera-trace.exe'
    agent.put(args.exe, exe)

    host = None
    if not args.no_host:
        host = subprocess.Popen(['sudo', str(HERE / 'hosttrace.sh'), str(args.seconds + 4), str(out.resolve()),
                                 str(args.qemu_pid or find_qemu())])
        while not (out / 'host.ready').exists():
            if host.poll() is not None:
                sys.exit('vtrace: hosttrace.sh failed')
            time.sleep(0.1)

    print(f'recording {args.seconds} s')
    rc, text = agent.run(exe, ['record', str(args.seconds), GUEST_DIR + r'\trace'], args.seconds + 600)
    print(text.rstrip())
    if rc:
        sys.exit(f'vtrace: venera-trace failed ({rc})')
    agent.get(GUEST_DIR + r'\trace\events.jsonl', out / 'events.jsonl')
    if host:
        host.wait()
    run = Run(out)
    print(f'{write_perfetto(run)}: open it in https://ui.perfetto.dev')
    cmd_summary(argparse.Namespace(runs=[out]))


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest='command', required=True)
    c = sub.add_parser('capture', help='record the guest and the host')
    c.add_argument('seconds', type=float)
    c.add_argument('run')
    c.add_argument('--qga', default=os.environ.get('QGA_SOCK'),
                   help='the guest agent socket (default: $QGA_SOCK, as docs/run-qemu.sh takes it)')
    c.add_argument('--qemu-pid', type=int, help='the VM\'s QEMU (default: the only one running)')
    c.add_argument('--no-host', action='store_true', help='the guest only (no sudo)')
    c.add_argument('--exe', default=str(ROOT / 'build/win/tests/win/venera-trace.exe'))
    c.set_defaults(func=cmd_capture)
    s = sub.add_parser('summary', help='latency and throughput, runs side by side')
    s.add_argument('runs', nargs='+')
    s.set_defaults(func=cmd_summary)
    f = sub.add_parser('perfetto', help='write RUN/trace.json')
    f.add_argument('run')
    f.set_defaults(func=cmd_perfetto)
    args = p.parse_args()
    args.func(args)


if __name__ == '__main__':
    main()
