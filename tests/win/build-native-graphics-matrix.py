#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Assemble package-DLL producers and the Windows history comparison test."""
import argparse
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile


def main():
    here = Path(__file__).resolve().parent
    root = here.parents[1]
    shared = runpy.run_path(str(here / 'build-native-graphics.py'))
    run, symbols, splice = (shared[key] for key in ('run', 'symbols', 'splice'))
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--build-dir', type=Path, default=Path('build/win'))
    p.add_argument('--icd-dir', type=Path, default=Path('build/icd'))
    args = p.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True)
    kit = out / 'windows'
    payload = kit / 'payload'
    payload.mkdir(parents=True)

    umd = '_ZN6venera3umd4logfENS0_8LogLevelEPKcz'
    dxvk = '_ZN4dxvk6Logger4warnERKNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEEE'
    kinds = {
        'd11': ('venera_d3d11', {'umd11_log': umd, 'dxvk_warn': dxvk}),
        'd12': ('venera_d3d12', {'umd12_log': umd, 'vkd_print': 'vkd3d_dbg_printf', 'vkd_flush': 'vkd3d_dbg_flush'}),
        'vk': ('venera_vulkan_virtio', {'vk_os': 'os_log_message', 'vk_log': 'mesa_log'}),
        'wgl': ('viogpu_wgl', {'wgl_os': 'os_log_message', 'wgl_log': 'mesa_log'}),
    }
    setup = args.build_dir.resolve() / 'setup'
    installer = setup / 'venera-setup.exe'
    wim = setup / 'payload.wim'
    if wim.read_bytes() not in installer.read_bytes():
        raise ValueError('payload.wim does not match the installer resource')
    names = [name + suffix + '.dll' for suffix in ('', '_32') for name, _ in kinds.values()]
    run('wimlib-imagex', 'extract', wim, '1', *names, '--dest-dir=' + str(payload))
    provenance = {'installer': str(installer), 'wim': str(wim), 'images': []}
    with tempfile.TemporaryDirectory(dir=out) as temp:
        temp = Path(temp)
        for arch, triplet, suffix, symbol_prefix, env_prefix, dll_root in (
            ('x64', 'x86_64-w64-mingw32-', '', '', 'VENERA_MINGW', args.build_dir / 'umd'),
            ('x86', 'i686-w64-mingw32-', '_32', '_', 'VENERA_MINGW32', args.build_dir / 'umd32/umd'),
        ):
            header = []
            for kind, (basename, functions) in kinds.items():
                name = basename + suffix + '.dll'
                dll = payload / name
                original = (dll_root if kind in ('d11', 'd12') else args.icd_dir) / name
                if dll.read_bytes() != original.read_bytes():
                    raise ValueError(f'Embedded {name} differs from {original}')
                table = symbols(dll)
                mapped = {label: table[symbol_prefix + symbol] for label, symbol in functions.items()}
                header.append(f'constexpr wchar_t image_{kind}[] = L"{name}";')
                header.extend(f'constexpr size_t {k} = 0x{v:x};' for k, v in mapped.items())
                provenance['images'].append({'arch': arch, 'image': name, 'bytes': dll.stat().st_size,
                    'build_path': str(original.resolve()), 'rvas': {k: f'0x{v:x}' for k, v in mapped.items()}})
            (temp / 'native-graphics-matrix-rvas.h').write_text('\n'.join(header) + '\n')
            run(triplet + 'g++', '-std=c++20', '-O2', '-municode', '-static', '-static-libgcc', '-static-libstdc++',
                '-isystem', os.environ[env_prefix + '_MCFG_INCLUDE'], '-L' + os.environ[env_prefix + '_MCFG_LIB'],
                '-I' + str(temp), here / 'native-graphics-package-matrix.cpp', '-o', kit / f'native-graphics-matrix-{arch}.exe')
        (kit / 'package-provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
        shutil.copyfile(here / 'native-graphics-matrix.md', kit / 'native-graphics-matrix.md')

        stage = temp / 'debug-channel'
        src = root / 'debug-channel'
        stage.mkdir()
        for name in ('Cargo.toml', 'Cargo.lock'):
            shutil.copyfile(src / name, stage / name)
        for crate in ('guest', 'host', 'protocol'):
            shutil.copytree(src / crate, stage / crate)
        tmf = Path('kmd/logging/generated/venera-recorder.tmf')
        (temp / tmf.parent).mkdir(parents=True)
        shutil.copyfile(root / tmf, temp / tmf)
        win = stage / 'guest/src/windows'
        for module, name in (('capture', 'native_graphics_package_matrix'), ('ipc', 'native_graphics_pipe'),
                             ('ipc', 'native_graphics_live_pair')):
            directory = win / module / 'tests'
            directory.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(here / (name.replace('_', '-') + '.rs'), directory / (name + '.rs'))
        anchor = '#[cfg(test)]\nmod tests {'
        splice(win / 'capture.rs', anchor, anchor + '\n    mod native_graphics_package_matrix;')
        splice(win / 'ipc.rs', anchor, '#[cfg(test)]\npub(crate) mod tests {\n    pub(crate) mod native_graphics_pipe;\n    pub(crate) mod native_graphics_live_pair;')
        target = Path(os.environ.get('CARGO_TARGET_DIR', str(out.parent / 'native-graphics-matrix-target'))).resolve()
        result = subprocess.run(['cargo', 'test', '--locked', '--offline', '--release', '--target', 'x86_64-pc-windows-gnu',
            '-p', 'venera-debug-guest', '--bin', 'venera-debug-guest', '--no-run', '--message-format=json'],
            cwd=stage, env=dict(os.environ, CARGO_TARGET_DIR=str(target)), stdout=subprocess.PIPE, text=True)
        messages = list(map(json.loads, result.stdout.splitlines()))
        for item in messages:
            if item.get('reason') == 'compiler-message':
                print(item['message'].get('rendered') or item['message']['message'], file=sys.stderr)
        result.check_returncode()
        exe, = (Path(item['executable']) for item in messages if item.get('executable') and item.get('profile', {}).get('test'))
        shutil.copyfile(exe, kit / 'native-graphics-matrix-history.exe')


if __name__ == '__main__':
    main()
