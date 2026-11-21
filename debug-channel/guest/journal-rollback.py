#!/usr/bin/env python3
"""Export a stopped journal snapshot to a fresh directory for legacy readers."""

import argparse
import json
import os
from pathlib import Path
import tempfile


LEGACY_LINE_BYTES = 1024 * 1024  # bf23745: includes the JSONL terminator
LEGACY_PAGE_BYTES = LEGACY_LINE_BYTES - 1024  # service history.read budget
CURRENT_LINE_BYTES = LEGACY_LINE_BYTES + 6 * 1024 + 256
LEGACY_SEGMENT_BYTES = 64 * 1024


def encode(value):
    return json.dumps(value, separators=(',', ':'), ensure_ascii=False).encode()


def restore_checkpoints(source, metadata):
    checkpoints = metadata.get("processes", {})
    for boot, buckets in metadata.pop("process_segments", {}).items():
        if str(int(boot)) != boot or not 0 <= int(boot) < 2**64 or boot in checkpoints:
            raise ValueError(f"invalid process checkpoint boot: {boot}")
        instances, interruptions = [], []
        progress = None
        for bucket, parts in sorted(buckets.items(), key=lambda item: int(item[0])):
            if str(int(bucket)) != bucket or not 0 <= int(bucket) < 2**32 or not parts:
                raise ValueError(f"invalid process bucket: {bucket}")
            for part, (length, alternate) in enumerate(parts):
                path = source / f"process-{boot}-{bucket}-{part}-{int(alternate)}.proc"
                with path.open('rb') as entry:
                    if (not 0 < length <= 1024 * 1024 or
                            os.fstat(entry.fileno()).st_size != length):
                        raise ValueError(f"invalid committed process extent: {path.name}")
                    for line in entry:
                        if not line.endswith(b'\n'):
                            raise ValueError(f"incomplete process row: {path.name}")
                        ((kind, data),) = json.loads(line).items()
                        if kind == 'progress' and progress is None and bucket == '0':
                            progress = data
                        elif kind == 'instance' and progress is not None:
                            instances.append(data)
                            if len(instances) > 65536:
                                raise ValueError(f"legacy process checkpoint capacity exceeded for boot {boot}")
                        elif kind == 'gap' and progress is not None and bucket == '0':
                            start, duration = data
                            interruptions.append({'from': start, 'through': start + duration})
                            if len(interruptions) > 16:
                                raise ValueError(f"legacy process checkpoint for boot {boot} exceeds 16 interruptions")
                        elif kind == 'source' and progress is not None and bucket == '0':
                            raise ValueError(f"legacy process checkpoint for boot {boot} cannot represent per-source progress: {data[0]!r}")
                        elif kind == 'context' and progress is not None:
                            raise ValueError(f"legacy process checkpoint for boot {boot} cannot represent pending context for PID {data[0]}")
                        else:
                            raise ValueError(f"invalid process row: {path.name}")
        if progress is None:
            raise ValueError(f"missing process progress for boot {boot}")
        captured_through, retired_before = progress
        processes = dict(instances=instances, interruptions=interruptions,
                         retired_before=retired_before)
        checkpoints[boot] = dict(through_filetime=captured_through, processes=processes)
    if checkpoints:
        for boot, checkpoint in checkpoints.items():
            processes = checkpoint['processes']
            if isinstance(processes, dict):
                if processes.get('pending_context') or processes.get('source_through'):
                    raise ValueError(f"legacy process checkpoint for boot {boot} cannot represent pending context or per-source progress")
                if len(processes['interruptions']) > 16:
                    raise ValueError(f"legacy process checkpoint for boot {boot} exceeds 16 interruptions")
                processes = processes['instances']
            if len(processes) > 65536:
                raise ValueError(f"legacy process checkpoint capacity exceeded for boot {boot}")
        metadata['processes'] = checkpoints


def committed_rows(source, segments, metadata):
    previous = 0
    for key, (length, alternate) in sorted(segments.items(), key=lambda item: int(item[0])):
        start = int(key)
        if not (str(start) == key and 0 < start <= metadata['high_water'] and length > 0):
            raise ValueError(f"invalid extent at {key}")
        ext = 'compact' if alternate else 'jsonl'
        with (source / f'{start:020}.{ext}').open('rb') as src:
            remaining = length
            while remaining:
                line = src.readline(min(remaining, CURRENT_LINE_BYTES + 1))
                if not line.endswith(b'\n') or len(line) > CURRENT_LINE_BYTES:
                    raise ValueError(f"invalid committed row in segment {key} after position {previous}")
                remaining -= len(line)
                row = json.loads(line)
                position = int(row['cursor']['position'])
                if (not start <= position <= metadata['high_water'] or position <= previous
                        or row['cursor']['journal_id'] != metadata['journal_id']):
                    raise ValueError(f"invalid cursor at position {position} in segment {key}")
                previous = position
                yield start, position, line, row


def scan_revisions(source, segments, metadata):
    oldest = None
    revisions = {}
    for _, position, _, row in committed_rows(source, segments, metadata):
        if oldest is None:
            oldest = position
        if (revision := row.get('revision')) is not None:
            target = revision['target']
            target_position = int(target['position'])
            gap = row['event'].get('gap', {})
            if (target['journal_id'] != metadata['journal_id'] or not 0 < target_position < position
                    or row['event']['type'] != 'gap' or gap.get('reason') != 'attribution_revision'
                    or gap.get('count') != '0' or gap.get('first_seq') is not None
                    or gap.get('last_seq') is not None):
                raise ValueError(f"invalid attribution revision at position {position}")
            revisions[target_position] = revision['attribution']
    return oldest, revisions


def write_segments(source, staging, segments, metadata, oldest, revisions):
    dest = None
    last_segment = None
    try:
        for segment, position, line, row in committed_rows(source, segments, metadata):
            # A revision marker keeps its cursor. A target already aged out has no retained log.
            if row.pop('revision', None) is not None:
                line = encode(row) + b'\n'
            if position in revisions:
                if row['event']['type'] != 'log':
                    raise ValueError(f"attribution revision targets non-log position {position}")
                attribution = revisions.pop(position)
                row['event']['record'].update(process=attribution['process'], ancestors=attribution['ancestors'])
                line = encode(row) + b'\n'
            if len(line) > LEGACY_LINE_BYTES:
                raise ValueError(f"position {position} needs {len(line)} bytes; legacy disk row limit is {LEGACY_LINE_BYTES}")
            record = {key: row[key] for key in ('cursor', 'boot_id', 'recorded_at_ms', 'startup', 'event')}
            empty_page = dict(range=dict(journal_id=metadata['journal_id'], oldest=str(oldest),
                                         high_water=str(metadata['high_water'])),
                              next=row['cursor'], missing=[], records=[])
            if max(len(line), len(encode(empty_page)) + len(encode(record))) > LEGACY_PAGE_BYTES:
                raise ValueError(f"position {position} exceeds legacy history page capacity ({LEGACY_PAGE_BYTES} bytes)")
            group = row['boot_id'], row['startup']
            if (segment != last_segment or group != last_group or count == 256
                    or size + len(line) > LEGACY_SEGMENT_BYTES):
                if dest is not None:
                    sync_file(dest)
                    dest.close()
                dest = (staging / f'{position:020}.jsonl').open('xb')
                last_segment, last_group, count, size = segment, group, 0, 0
            dest.write(line)
            count += 1
            size += len(line)
        if dest is not None:
            sync_file(dest)
    finally:
        if dest is not None:
            dest.close()


def sync_file(file):
    file.flush()
    os.fsync(file.fileno())


def export(source, destination):
    source = source.resolve()
    destination = destination.parent.resolve() / destination.name
    if destination.is_relative_to(source):
        raise ValueError("choose a sibling directory for the export")
    if os.path.lexists(destination):
        raise ValueError(f"export destination already exists: {destination}")
    metadata = json.loads((source / "metadata.json").read_bytes())
    fragments = metadata.pop('graphics_fragments', {})
    if fragments:
        blocked = ', '.join(f'graphics fragments for boot {boot} ({extent[0]} bytes)'
                            for boot, extent in sorted(fragments.items()))
        raise ValueError(f"legacy format cannot represent {blocked}")
    segments = metadata.pop("segments")
    restore_checkpoints(source, metadata)
    encoded = encode(metadata)
    if len(encoded) > LEGACY_LINE_BYTES:
        boots = ', '.join(metadata.get('processes', {})) or 'none'
        raise ValueError(f"legacy journal manifest needs {len(encoded)} bytes (limit {LEGACY_LINE_BYTES}); process checkpoint boots: {boots}")
    oldest, revisions = scan_revisions(source, segments, metadata)
    with tempfile.TemporaryDirectory(prefix='.journal-rollback-', dir=destination.parent) as temp:
        staging = Path(temp) / 'journal'
        staging.mkdir()
        write_segments(source, staging, segments, metadata, oldest, revisions)
        with (staging / 'metadata.json').open('xb') as out:
            out.write(encoded)
            sync_file(out)
        if os.path.lexists(destination):
            raise ValueError(f"export destination already exists: {destination}")
        staging.rename(destination)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    try:
        export(args.source, args.destination)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"journal rollback: {error}\n")
