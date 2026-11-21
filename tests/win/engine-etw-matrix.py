#!/usr/bin/env python3
"""Run the source-runtime PE probes built by build-json-probes.py --etw-fixture."""
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile

probes = Path(sys.argv[1]).resolve()
runner = sys.argv[2:]  # e.g. wine
shader = b'mov r0, r1\n' * 8000


def messages(path):
    raw = path.read_bytes() if path.exists() else b''
    groups = {}
    while raw:
        event, version, _, level, _, _, keyword, length = struct.unpack_from('<HBBBBHQI', raw)
        assert (version, level, keyword) == (1, 4, 1)
        payload, raw = raw[20:20+length], raw[20+length:]
        time, created, instance, seq, index, count = struct.unpack_from('<QQQQII', payload)
        severity, channel, text = payload[40:].split(b'\0', 2)
        parts = groups.setdefault((event, severity, channel, time, created, instance, seq), [None] * count)
        assert parts[index] is None
        parts[index] = text
    return [(k[:3], b''.join(v)) for k, v in groups.items()]


for engine in ('dxvk', 'vkd3d', 'mesa', 'venera'):
    for transport in ('on', 'off', 'fail'):
        for file in ((True, False) if engine != 'mesa' else (False,)):
            with tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                env = dict(os.environ, TEST_ETW=transport, TEST_SHADER_TEXT='1', DXVK_LOG_LEVEL='warn',
                           DXVK_LOG_PATH='.' if file else 'none', VKD3D_DEBUG='err',
                           VKD3D_SHADER_DEBUG='warn', MESA_LOG='windbg', MESA_LOG_LEVEL='debug')
                env.pop('VKD3D_LOG_FILE', None)
                env.pop('VENERA_LOG', None)
                env['VENERA_VERBOSE'] = '0'
                if not file and engine == 'vkd3d':
                    env.pop('TEST_SHADER_TEXT')  # Wine's console formats this branch in a 4096-byte buffer.
                if file:
                    env['VKD3D_LOG_FILE'] = 'shader.log'
                    env['VENERA_LOG'] = 'umd.log'
                    if engine == 'venera':
                        (root / 'umd.log').write_bytes(b'existing\n')
                subprocess.run([*runner, str(probes / f'source-runtime-{engine}.exe')], cwd=temp,
                               env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if engine == 'dxvk':
                    expect = [((7,b'err',b'default'),b'source-runtime dxvk error literal'),
                              ((7,b'warn',b'default'),b'source-runtime dxvk warning literal'),
                              ((7,b'warn',b'default'),shader)]
                    log = root / 'source-runtime-dxvk_source-runtime-dxvk.log'
                    if file:
                        assert log.read_text() == ('err:   source-runtime dxvk error literal\n'
                            'warn:  source-runtime dxvk warning literal\n' + 'warn:  mov r0, r1\n' * 8000)
                elif engine == 'vkd3d':
                    expect = []
                    for channel, levels in [('api',['err']), ('shader',['err','info','fixme','warn'])]:
                        for level in levels:
                            word = {'err':'error','warn':'warning'}.get(level,level)
                            text = f'{channel}_messages: source-runtime vkd3d {channel} {word} literal\n'.encode()
                            expect.append(((8,level.encode(),channel.encode()),text))
                    if file:
                        expect.append(((8,b'warn',b'shader'),b'compile: ' + shader))
                    log = root / 'shader.log'
                    if file:
                        normalized = re.sub(r'(?m)^[0-9a-f]{4,8}:', '', log.read_text())
                        assert normalized.encode() == b''.join(k[1]+b':'+v for k,v in expect)
                elif engine == 'venera':
                    log = root / 'umd.log'
                    if file:
                        lines = log.read_text().splitlines()
                        assert lines[0] == 'existing'
                        assert [re.sub(r'^\d\d:\d\d:\d\d\.\d{3} venera_d3d11\[\d+:\d+\] ', '', line) for line in lines[1:]] == [
                            f'{code} source-runtime venera {word} literal' for code,word in zip('EWID',('error','warning','info','debug'))]
                    assert log.exists() == file
                    assert (root / 'events.bin').exists() == (transport == 'on')
                    print(engine, transport, 'file', file, 'passed', flush=True)
                    continue
                else:
                    expect = []
                    for level, word in [('error','error'),('warning','warning'),('info','info'),('debug','debug')]:
                        expect.append(((10,level.encode(),b'MESA'),f'MESA: {level}: source-runtime mesa {word} literal\n'.encode()))
                    expect.extend([((10,b'info',b'MESA-VIRTIO'), b'MESA-VIRTIO: info: source-runtime mesa virtio info literal\n'),
                                   ((9,b'unknown',b'default'), b'source-runtime mesa os_log_message literal\n'),
                                   ((9,b'unknown',b'default'), b'MESA-VIRTIO: source-runtime mesa os virtio literal\n')])
                if engine != 'mesa':
                    assert log.exists() == file
                actual = messages(root / 'events.bin')
                expected = expect if transport == 'on' else []
                assert actual == expected, (engine, [(k, v[:96], len(v)) for k,v in actual])
                print(engine, transport, 'file', file, 'passed', flush=True)
