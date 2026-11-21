#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Assemble the Windows graphics acceptance kit and Linux archive executable."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def run(*args, **kwargs):
    return subprocess.run([str(x) for x in args], check=True, **kwargs)


def symbols(path):
    tool = 'x86_64-w64-mingw32-'
    info = run(tool + 'objdump', '-p', path, capture_output=True, text=True).stdout
    base = int(re.search(r'^ImageBase\s+([0-9a-f]+)$', info, re.M)[1], 16)
    listing = run(tool + 'nm', '--defined-only', path, capture_output=True, text=True).stdout
    return {name: int(address, 16) - base for address, name in
            re.findall(r'^([0-9a-f]+) T (\S+)$', listing, re.M)}


def splice(file, anchor, text):
    before = file.read_text()
    if before.count(anchor) != 1:
        raise ValueError(f'QA insertion point changed: {file}')
    file.write_text(before.replace(anchor, text, 1))


def main():
    here = Path(__file__).resolve().parent
    src = here.parents[1] / 'debug-channel'
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build-dir', type=Path, default=Path('build/win'))
    parser.add_argument('--mesa-build-dir', type=Path, default=Path('build/icd/meson'))
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True)
    kit = out / 'windows'
    payload = kit / 'payload'
    payload.mkdir(parents=True)
    images = {
        'venera_d3d11.dll': {
            'dxvk_warn': '_ZN4dxvk6Logger4warnERKNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEEE',
            'umd_log': '_ZN6venera3umd4logfENS0_8LogLevelEPKcz',
        },
        'venera_d3d12.dll': {'vkd_print': 'vkd3d_dbg_printf', 'vkd_flush': 'vkd3d_dbg_flush'},
        'venera_vulkan_virtio.dll': {'mesa_os': 'os_log_message', 'mesa_log_rva': 'mesa_log'},
    }
    setup = args.build_dir / 'setup'
    installer = (setup / 'venera-setup.exe').read_bytes()
    wim = setup / 'payload.wim'
    if wim.read_bytes() not in installer:
        raise ValueError('payload.wim does not match the installer resource')
    with tempfile.TemporaryDirectory(dir=out) as temp:
        stage = Path(temp) / 'debug-channel'
        stage.mkdir()
        run('wimlib-imagex', 'extract', wim, 1, *images, '--dest-dir=' + str(payload))
        rvas = []
        for dll, functions in images.items():
            path = payload / dll
            original = (args.mesa_build_dir.parent if 'vulkan' in dll else args.build_dir / 'umd') / dll
            if path.read_bytes() != original.read_bytes():
                raise ValueError(f'Embedded {dll} differs from {original}')
            mapped = symbols(path)
            rvas.extend(f'constexpr size_t {name} = 0x{mapped[symbol]:x};' for name, symbol in functions.items())
        (out / 'package-provenance.txt').write_text(
            f'Installer: {(setup / "venera-setup.exe").resolve()} ({len(installer)} bytes)\n'
            f'Embedded WIM: {wim.resolve()}\n'
            + ''.join(f'{dll}: {(payload / dll).stat().st_size} bytes\n' for dll in images)
            + '\n'.join(rvas) + '\n')
        (stage / 'native-graphics-rvas.h').write_text('\n'.join(rvas) + '\n')
        run('x86_64-w64-mingw32-g++', '-std=c++20', '-O2', '-municode', '-static',
            '-static-libgcc', '-static-libstdc++', '-isystem', os.environ['VENERA_MINGW_MCFG_INCLUDE'],
            '-L' + os.environ['VENERA_MINGW_MCFG_LIB'], '-I' + str(stage),
            here / 'native-graphics-package.cpp', '-o', kit / 'native-graphics-package.exe')
        run(sys.executable, here / 'build-json-probes.py', '--build-dir', args.build_dir,
            '--mesa-build-dir', args.mesa_build_dir, '--output-dir', stage / 'probes')
        for label in ('dxvk', 'vkd3d', 'mesa', 'venera'):
            name = f'source-runtime-{label}.exe'
            shutil.copyfile(stage / 'probes' / name, kit / name)

        for file in ('Cargo.toml', 'Cargo.lock'):
            shutil.copyfile(src / file, stage / file)
        for crate in ('guest', 'protocol', 'host'):
            shutil.copytree(src / crate, stage / crate)
        tmf = Path('kmd/logging/generated/venera-recorder.tmf')
        (stage.parent / tmf.parent).mkdir(parents=True)
        shutil.copyfile(src.parent / tmf, stage.parent / tmf)
        win = stage / 'guest/src/windows'
        (win / 'capture/tests').mkdir(parents=True, exist_ok=True)
        (win / 'ipc/tests').mkdir(parents=True, exist_ok=True)
        shutil.copyfile(here / 'native-graphics-history.rs', win / 'capture/tests/native_graphics_history.rs')
        shutil.copyfile(here / 'native-graphics-pipe.rs', win / 'ipc/tests/native_graphics_pipe.rs')
        anchor = '#[cfg(test)]\nmod tests {'
        splice(win / 'capture.rs', anchor,
               '#[cfg(test)]\nmod tests {\n    mod native_graphics_history;')
        splice(win / 'ipc.rs', anchor,
               '#[cfg(test)]\npub(crate) mod tests {\n    pub(crate) mod native_graphics_pipe;')
        archive = stage / 'graphics-archive'
        archive.mkdir()
        shutil.copyfile(here / 'native-graphics-archive.rs', archive / 'main.rs')
        (archive / 'Cargo.toml').write_text('''[package]
name = "native-graphics-archive"
version = "0.0.0"
edition = "2021"
publish = false

[[bin]]
name = "native-graphics-archive"
path = "main.rs"

[dependencies]
rusqlite = { version = "0.32", features = ["bundled"] }
serde_json = "1"
time = { version = "0.3", features = ["formatting", "parsing"] }
venera-debug-protocol = { path = "../protocol" }

[workspace]
''')
        env = dict(os.environ, CARGO_TARGET_DIR=os.environ.get('CARGO_TARGET_DIR', str(out.parent / 'native-graphics-target')))
        run('cargo', 'build', '--offline', '--release', cwd=archive, env=env)
        shutil.copy(Path(env['CARGO_TARGET_DIR']) / 'release/native-graphics-archive', out)
        command = ['cargo', 'test', '--locked', '--offline', '--release', '--target', 'x86_64-pc-windows-gnu',
                   '-p', 'venera-debug-guest', '--bin', 'venera-debug-guest', '--no-run', '--message-format=json']
        built = subprocess.run(command, cwd=stage, env=env, stdout=subprocess.PIPE, text=True)
        messages = list(map(json.loads, built.stdout.splitlines()))
        for msg in messages:
            if msg.get('reason') == 'compiler-message':
                print(msg['message'].get('rendered') or msg['message']['message'], file=sys.stderr)
        built.check_returncode()
        exe, = (Path(msg['executable']) for msg in messages
                if msg.get('executable') and msg.get('profile', {}).get('test'))
        shutil.copyfile(exe, kit / 'native-graphics-history.exe')


if __name__ == '__main__':
    main()
