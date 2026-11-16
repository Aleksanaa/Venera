#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

panel = Path(__file__).resolve().parents[1]
ids = dict(re.findall(r'#define (IDS_\w+) (\d+)', (panel / 'strings.h').read_text()))
assert len(ids) == len(set(ids.values()))
catalogs = []
for block in (panel / 'strings.rc').read_text().split('STRINGTABLE')[1:]:
    catalog = dict((key, json.loads(value)) for key, value in re.findall(r'(IDS_\w+) ("(?:[^"\\]|\\.)*")', block))
    assert set(catalog) == set(ids), 'catalog keys'
    assert all(catalog.values()), 'empty message'
    catalogs.append(catalog)
assert len(catalogs) == 2
for key in ids:
    placeholders = [sorted(re.findall(r'\{\d\}', catalog[key])) for catalog in catalogs]
    assert placeholders[0] == placeholders[1], key
    for catalog in catalogs:
        assert not re.search(r'[{}]', re.sub(r'\{\d\}', '', catalog[key])), key
for source in panel.rglob('*.cpp'):
    for key in re.findall(r'IDS_\w+', source.read_text()):
        assert key in ids, (source, key)

with tempfile.TemporaryDirectory(prefix='panel-locale-', dir=os.environ.get('TMPDIR')) as folder:
    folder = Path(folder)
    fixture = '#include "strings.h"\n'
    for name, catalog in zip(['english', 'chinese'], catalogs):
        fixture += f'const wchar_t* {name}(unsigned id) {{ switch(id) {{\n'
        for key, value in catalog.items():
            fixture += f'case {key}: return L{json.dumps(value, ensure_ascii=False)};\n'
        fixture += 'default: return L""; }}\n'
    (folder / 'locale_catalog_fixture.h').write_text(fixture)
    subprocess.run([os.environ.get('CXX', 'c++'), '-std=c++17', '-Wall', '-Wextra', '-Werror',
                    '-iquote' + str(folder), '-iquote' + str(panel), str(panel / 'locale_core.cpp'),
                    str(panel / 'tests/locale.cpp'), '-o', str(folder / 'locale-test')], check=True)
    subprocess.run([str(folder / 'locale-test')], check=True)
print(f'Validated {len(ids)} bilingual messages and native locale behavior.')
