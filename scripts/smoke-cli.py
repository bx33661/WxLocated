#!/usr/bin/env python3
"""Exercise the shipped CLI with isolated, synthetic wxapkg data; no WeChat required."""

import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import uuid


def package_bytes(files):
    index_size = 4 + sum(12 + len(name.encode()) for name in files)
    offset = 14 + index_size
    index = struct.pack('>I', len(files))
    body = b''
    for name, content in files.items():
        encoded = name.encode()
        index += struct.pack('>I', len(encoded)) + encoded + struct.pack('>II', offset, len(content))
        body += content
        offset += len(content)
    return b'\xbe' + bytes(4) + struct.pack('>II', index_size, len(body)) + b'\xed' + index + body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', required=True, type=Path)
    args = parser.parse_args()
    executable = args.cli.resolve(strict=True)
    app_id = 'wx' + uuid.uuid4().hex[:16]
    with tempfile.TemporaryDirectory(prefix='WxLocated 中文 smoke ') as temporary:
        root = Path(temporary)
        package = root / 'WeChat Files' / 'wxid_smoke_account' / 'Applet' / app_id / '2' / '__APP__.wxapkg'
        package.parent.mkdir(parents=True)
        package.write_bytes(package_bytes({
            'app.js': b'App({});\n',
            'app-config.json': json.dumps({'pages': ['pages/index/index'], 'global': {'window': {
                'navigationBarTitleText': 'Windows smoke',
            }}}).encode(),
            'pages/index/index.js': b'Page({});\n',
        }))
        environment = os.environ.copy()
        environment['WXLOCATED_SMOKE_CACHE'] = str(root / 'WeChat Files')
        scan = ['--scan-path', '%WXLOCATED_SMOKE_CACHE%/*/Applet']

        def run(*arguments):
            process = subprocess.run([str(executable), *map(str, arguments)], env=environment,
                                     capture_output=True, encoding='utf-8', timeout=60)
            if process.returncode:
                raise RuntimeError(f'{arguments[0]} failed ({process.returncode}): {process.stdout}\n{process.stderr}')
            return json.loads(process.stdout)

        listing = run('list', *scan)
        assert any(app['app_id'] == app_id for app in listing['miniapps'])
        info = run('info', app_id, *scan)
        assert info['metadata']['app_id'] == app_id
        exported = run('export', app_id, '--out', root / '导出 output', '--redact', *scan)
        material_root = Path(exported['root'])
        assert (material_root / 'source/app.js').read_bytes() == b'App({});\n'
        assert any((material_root / 'raw').rglob('*.wxapkg'))
        for manifest in material_root.rglob('*.json'):
            if manifest.name in ('material.json', 'WxLocated-decompilation.json'):
                text = manifest.read_text(encoding='utf-8')
                json.loads(text)
                assert 'wxid_smoke_account' not in text
        trace = run('trace', material_root, r'source\app.js:1')
        assert trace['match']
        reused = run('export', app_id, '--out', root / '导出 output', '--redact', '--reuse', *scan)
        assert reused['reused'] is True
        run('overlay', 'init', material_root, root / '语义层 work', '--name', 'CON')
        assert (root / '语义层 work/overlay.json').is_file()
        print('CLI smoke passed: scan, info, export, redact, trace, reuse, overlay init (Unicode paths).')


if __name__ == '__main__':
    main()
