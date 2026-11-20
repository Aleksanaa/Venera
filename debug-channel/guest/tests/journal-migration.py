#!/usr/bin/env python3
"""Exercise migration and rollback with the published legacy journal reader."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

GUEST = Path(__file__).resolve().parents[1]
LEGACY = 'bf23745'
PROBE = '''
mod ancestry;
mod journal;
#[path = "KERNEL_EVENTS"] mod kernel_events;
use journal::{Journal, RetentionPolicy};
use venera_debug_protocol::v1::{HistoryEvent, Gap, HistoryRead};
fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let mut j = Journal::open(&args[2], "migration", RetentionPolicy {
        runtime_ms: 10, max_bytes: args[3].parse().unwrap(), ..Default::default()
    })?;
    if args[1] == "seed" || args[1] == "resume" {
        let count = if args[1] == "seed" {50} else {1};
        for n in 0..count {
            j.append("boot", if n%2==0 {0} else {9}, false, HistoryEvent::Gap {gap: Gap {
                stream_id:"origin".into(), reason:format!("row {n}"), count:None,
                first_seq:None, last_seq:None,
            }})?;
        }
    }
    if args[1] == "prune" { j.prune(12)?; }
    if args[1] == "process" {
        let checkpoint = j.process_checkpoint(100)?;
        let (a, _) = checkpoint.processes.observed(9, 111).unwrap();
        let (b, _) = checkpoint.processes.observed(9, 121).unwrap();
        println!("{}", serde_json::json!([checkpoint.through_filetime, a.start_key, b.start_key]));
        return j.close();
    }
    CURRENT_ACTIONS
    let bytes = if args[1] == "wide-read" { 1024 * 1024 - 1024 } else { 65536 };
    let page = j.read(HistoryRead{after:None,through:None,limit:100},bytes)?;
    println!("{}",serde_json::to_string(&page)?);
    j.close()
}
'''

CURRENT_ACTIONS = r'''
    if args[1] == "seed-graphics" {
        let boot = 134_393_633_875_000_000u64;
        let source = journal::ImportSource { id: "left.etl".into(), boot_filetime: boot };
        j.discover_imports(vec![source.clone()])?;
        let mut decoder = engine_events::Decoder::default();
        let mut payload = Vec::new();
        for word in [boot + 10, boot + 1, 123, 456] {
            payload.extend(word.to_le_bytes());
        }
        payload.extend(0u32.to_le_bytes());
        payload.extend(2u32.to_le_bytes());
        payload.extend(b"warn\0default\0");
        payload.extend(vec![b'a'; 60 * 1024]);
        assert!(decoder.decode(&payload, boot, 7, 1, 4, 42, 43)?.is_none());
        j.complete_graphics_import(&source, None, boot, 1, Some(decoder.checkpoint(boot)))?;
        j.close()?;
        let reopened = Journal::open(&args[2], "migration", RetentionPolicy::default())?;
        let decoder = engine_events::Decoder::restore(boot, &reopened.graphics_fragments(boot)?)?;
        println!("{}", decoder.incomplete());
        return reopened.close();
    }
'''


def run_fixture(root):
    target = root / 'target'
    for variant in ['legacy', 'current']:
        tree = root / variant
        crate = tree / 'debug-channel/guest'
        src = crate / 'src'
        src.mkdir(parents=True)
        modules = ['ancestry.rs', 'journal.rs', 'journal_space.rs']
        if variant == 'current':
            modules.extend(p.name for p in (GUEST / 'src').glob('journal_*.rs')
                           if p.name not in modules)
            modules.extend(['user_events.rs', 'engine_events.rs', 'engine_events'])
        for name in modules:
            if variant == 'legacy':
                (src / name).write_bytes(legacy_file(f'guest/src/{name}'))
            else:
                (src / name).symlink_to(GUEST / 'src' / name)
        kernel = GUEST / 'src/kernel_events.rs'
        protocol = GUEST.parent / 'protocol'
        if variant == 'legacy':
            kernel = src / 'kernel_events.rs'
            kernel.write_bytes(legacy_file('guest/src/kernel_events.rs'))
            tmf = tree / 'kmd/logging/generated/venera-recorder.tmf'
            tmf.parent.mkdir(parents=True)
            tmf.write_bytes(subprocess.check_output(
                ['git', 'show', f'{LEGACY}:kmd/logging/generated/venera-recorder.tmf'], cwd=GUEST))
            protocol = crate / 'protocol'
            for name in subprocess.check_output(
                ['git', 'ls-tree', '-r', '--name-only', LEGACY, '--', 'debug-channel/protocol'],
                cwd=GUEST.parent.parent, text=True,
            ).splitlines():
                path = crate / name.removeprefix('debug-channel/')
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(legacy_file(name.removeprefix('debug-channel/')))
        main = PROBE.replace('KERNEL_EVENTS', str(kernel)).replace(
            'CURRENT_ACTIONS', CURRENT_ACTIONS if variant == 'current' else '')
        if variant == 'current':
            main = 'mod user_events;\nmod engine_events;\n' + main
        (src / 'main.rs').write_text(main)
        (crate / 'Cargo.toml').write_text('''[package]
name="VARIANT"
version="0.1.0"
edition="2021"
[workspace]
[workspace.package]
version="0.1.0"
edition="2021"
license="MIT"
[workspace.dependencies]
serde={version="1",features=["derive"]}
serde_json="1"
[dependencies]
libc="0.2"
serde={version="1",features=["derive"]}
serde_json="1"
venera-debug-protocol={path=PROTOCOL}
'''.replace('VARIANT', variant).replace('PROTOCOL', json.dumps(str(protocol))))
        subprocess.run(['cargo', 'build', '--offline', '--target-dir', target], cwd=crate, check=True)

    def probe(variant, action, directory, budget=1 << 20, check=True):
        run = subprocess.run([target / 'debug' / variant, action, directory, str(budget)], capture_output=True)
        if check:
            assert run.returncode == 0, (variant, action, run.stderr)
        return json.loads(run.stdout) if check else run

    original, snapshot, rollback = (root / n for n in ('original', 'snapshot', 'rollback'))
    expected = probe('legacy', 'seed', original)
    assert len(expected['records']) == 50
    metadata_path = original / 'metadata.json'
    metadata = json.loads(metadata_path.read_bytes())
    metadata['recent_boots'].append('100')
    metadata['processes'] = {'100': {
        'through_filetime': 125,
        'processes': [
            dict(identity=dict(pid=9, boot_time_filetime='100', start_time_filetime=str(start),
                               start_key=str(key)), parent=None, session=None, ended=end)
            for start, key, end in [(110, 1, 120), (120, 2, None)]
        ],
    }}
    metadata_path.write_text(json.dumps(metadata))
    assert probe('legacy', 'process', original) == [125, '1', '2']
    shutil.copytree(original, snapshot)
    segments = {p.name: p.read_bytes() for p in original.glob('*.jsonl')}
    unit = os.statvfs(snapshot).f_frsize
    budget = sum(max(p.stat().st_blocks*512, (p.stat().st_size+unit-1)//unit*unit) for p in snapshot.iterdir())
    failed = probe('current', 'read', snapshot, budget, check=False)
    assert failed.returncode and b'capacity' in failed.stderr, failed.stderr
    assert probe('legacy', 'read', snapshot) == expected
    # A complete migration manifest can be on disk while metadata.json still names
    # the legacy generation. Capture that exact boundary using an isolated donor.
    donor = root / 'donor'
    shutil.copytree(original, donor)
    assert probe('current', 'read', donor) == expected
    shutil.copyfile(donor / 'metadata.json', snapshot / 'metadata.pending')
    files = {p.name: p.stat().st_ino for p in snapshot.glob('*.jsonl')}
    tail = sorted(snapshot.glob('*.jsonl'))[-1]
    with tail.open('ab') as f:
        f.write(b'{interrupted')
    assert probe('current', 'read', snapshot) == expected
    assert probe('current', 'process', snapshot) == [125, '1', '2']
    assert 'processes' not in json.loads((snapshot / 'metadata.json').read_bytes())
    assert segments == {p.name: p.read_bytes() for p in snapshot.glob('*.jsonl')}
    assert files == {p.name: p.stat().st_ino for p in snapshot.glob('*.jsonl')}
    kept = probe('current', 'prune', snapshot)
    assert kept['records'] == expected['records'][1::2]
    subprocess.run([sys.executable, GUEST / 'journal-rollback.py', snapshot, rollback], check=True)
    assert probe('legacy', 'read', rollback) == kept
    assert probe('legacy', 'process', rollback) == [125, '1', '2']
    assert probe('legacy', 'resume', rollback)['range']['high_water'] == '51'
    print(f'{LEGACY}: 50 original records, {budget}-byte migration pressure, 25 rollback records, resumed at 51')
    rollback_cases(root, probe)


def legacy_file(name):
    return subprocess.check_output(['git', 'show', f'{LEGACY}:debug-channel/{name}'], cwd=GUEST)


def rollback_cases(root, probe):
    journal_id = 'rollback-fixture'
    boot = '134393633875000000'

    def cursor(position):
        return dict(journal_id=journal_id, position=str(position))

    def row(position, event):
        return dict(cursor=cursor(position), boot_id=boot, recorded_at_ms='1',
                    startup=True, event=event)

    def log(text):
        return dict(type='log', record=dict(stream_id='log', seq='1', source='etw',
                    component='guest', raw_level='info', level='info', process=dict(pid=42),
                    observed_at='2026-11-17T04:29:48Z', text=text))

    def revision(position, process, ancestors):
        return dict(**row(position, dict(type='gap', gap=dict(stream_id=journal_id,
                    reason='attribution_revision', count='0'))), revision=dict(
                    target=cursor(1), attribution=dict(process=process, ancestors=ancestors)))

    def snapshot(name, groups, **extras):
        directory = root / name
        directory.mkdir()
        segments = {}
        for rows in groups:
            position = rows[0]['cursor']['position']
            data = b''.join(encode(record) + b'\n' for record in rows)
            segments[position] = [len(data), True]
            (directory / f'{int(position):020}.compact').write_bytes(data + b'uncommitted')
        (directory / 'metadata.json').write_bytes(encode(dict(journal_id=journal_id,
            high_water=7, recent_boots=[boot], segments=segments, **extras)))
        return directory

    def export(source, error=None):
        destination = source.with_name(source.name + '-legacy')
        before = {path.name: path.read_bytes() for path in source.iterdir()}
        run = subprocess.run([sys.executable, GUEST / 'journal-rollback.py', source, destination],
                             capture_output=True)
        assert before == {path.name: path.read_bytes() for path in source.iterdir()}
        if error:
            assert run.returncode and error in run.stderr, run.stderr
            assert not destination.exists()
        else:
            assert run.returncode == 0, run.stderr
        return destination

    process = dict(pid=42, start_time_filetime='134393633875000200', start_key='4201', exe_path=None)
    ancestor = dict(pid=7, start_time_filetime='134393633875000100', start_key='701', exe_path=None)
    # The original first segment fits 64 KiB; the attribution snapshot grows it.
    text = '日志\\\n' + 'x' * 64980
    first, second = row(1, log(text)), row(2, dict(type='gap', gap=dict(stream_id='log', reason='gap')))
    first_revision = revision(3, process, [])
    final_revision = revision(5, process, [ancestor])
    final_log = row(1, log(text))
    final_log['event']['record'].update(process=process, ancestors=[ancestor])
    assert len(encode(first)) + len(encode(second)) + 2 <= 65536
    assert len(encode(final_log)) + len(encode(second)) + 2 > 65536
    revised = snapshot('revised', [[first, second], [first_revision], [final_revision]])
    page = probe('legacy', 'wide-read', export(revised))
    assert page['range'] == dict(journal_id=journal_id, oldest='1', high_water='7')
    assert page['next'] == cursor(7)
    assert [record['cursor']['position'] for record in page['records']] == ['1', '2', '3', '5']
    assert page['missing'] == [dict(first='4', last='4', reason='retention_expired'),
                               dict(first='6', last='7', reason='retention_expired')]
    saved = page['records'][0]['event']['record']
    assert saved['text'] == text and saved['process'] == process and saved['ancestors'] == [ancestor]
    assert [page['records'][i]['event']['gap']['reason'] for i in (2, 3)] == ['attribution_revision'] * 2

    graphics = snapshot('graphics', [[second]])
    assert probe('current', 'seed-graphics', graphics, budget=64 << 20) == 1
    export(graphics, f'graphics fragments for boot {boot}'.encode())
    # The legacy reader accepts the history but silently ignores fragment state.
    old_view = root / 'graphics-reader-control'
    old_view.mkdir()
    shutil.copyfile(graphics / 'metadata.json', old_view / 'metadata.json')
    for fragment in graphics.glob('*.frag'):
        shutil.copyfile(fragment, old_view / fragment.name)
    (old_view / '00000000000000000002.jsonl').write_bytes(encode(second) + b'\n')
    old_page = probe('legacy', 'read', old_view)
    assert old_page['range']['high_water'] == '7'
    assert old_page['records'][0]['event']['gap']['reason'] == 'gap'

    too_large = row(5, log('x' * (1024 * 1024 - 2000)))
    too_large['provenance'] = dict(source=dict(id='\x01' * 1024, boot_filetime=int(boot)),
        ordinal=1, source_filetime=int(boot), complete=True)
    large = snapshot('large-row', [[second], [too_large]])
    assert probe('current', 'wide-read', large, budget=2 << 20)['records'][1]['cursor'] == cursor(5)
    export(large, b'legacy disk row limit')
    old_large = root / 'legacy-large-row-control'
    shutil.copytree(large, old_large)
    for key, (length, _) in json.loads((large / 'metadata.json').read_bytes())['segments'].items():
        raw = (large / f'{int(key):020}.compact').read_bytes()
        (old_large / f'{int(key):020}.jsonl').write_bytes(raw[:length])
    failed = probe('legacy', 'wide-read', old_large, budget=2 << 20, check=False)
    assert failed.returncode and b'journal entry exceeds frame capacity' in failed.stderr
    checkpoint = snapshot('checkpoint', [[second]], process_segments={boot: {'0': []}})
    process_file = checkpoint / f'process-{boot}-0-0-0.proc'
    process_file.write_bytes(b'{"progress":[10,0]}\n' + encode({'source': ['capture.etl', 10]}) + b'\n')
    metadata = json.loads((checkpoint / 'metadata.json').read_bytes())
    metadata['process_segments'][boot]['0'] = [[process_file.stat().st_size, False]]
    (checkpoint / 'metadata.json').write_bytes(encode(metadata))
    export(checkpoint, f'process checkpoint for boot {boot}'.encode())

    large_checkpoint = snapshot('large-checkpoint', [[second]])
    metadata = json.loads((large_checkpoint / 'metadata.json').read_bytes())
    entries = [dict(identity=dict(pid=42, boot_time_filetime=boot,
                    start_time_filetime=str(int(boot) + index), exe_path=None),
                    parent=None, session=None, ended=None) for index in range(8000)]
    chunks = [b'{"progress":[134393633875008000,0]}\n']
    parts = []
    for index in range(0, len(entries), 4000):
        chunks.extend(encode({'instance': entry}) + b'\n' for entry in entries[index:index + 4000])
        data = b''.join(chunks)
        (large_checkpoint / f'process-{boot}-0-{len(parts)}-0.proc').write_bytes(data)
        parts.append([len(data), False])
        chunks.clear()
    metadata['process_segments'] = {boot: {'0': parts}}
    (large_checkpoint / 'metadata.json').write_bytes(encode(metadata))
    export(large_checkpoint, f'process checkpoint boots: {boot}'.encode())
    old_checkpoint = root / 'legacy-large-checkpoint-control'
    old_checkpoint.mkdir()
    metadata['processes'] = {boot: dict(through_filetime=int(boot) + 8000,
        processes=dict(instances=entries, retired_before=0, interruptions=[]))}
    metadata.pop('process_segments')
    (old_checkpoint / 'metadata.json').write_bytes(encode(metadata))
    failed = probe('legacy', 'read', old_checkpoint, check=False)
    assert failed.returncode and b'journal metadata exceeds capacity' in failed.stderr
    print(f'{LEGACY}: revised attribution and marker positions preserved; graphics, row and checkpoint blockers preserved snapshots')


def encode(value):
    return json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode()


if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='journal-migration-') as directory:
        run_fixture(Path(directory))
