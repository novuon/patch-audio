#!/usr/bin/env python3
"""Add non-SPDX Slint and vendored Signalsmith notices to cargo-about output."""
import json
from pathlib import Path
import sys

metadata_path, output_path = map(Path, sys.argv[1:])
metadata = json.loads(metadata_path.read_text())
slint_ref = 'LicenseRef-Slint-Royalty-free-2.0'
target_ids = {node['id'] for node in (metadata.get('resolve') or {}).get('nodes', [])}
packages = sorted(
    (p['name'], p['version']) for p in metadata['packages']
    if p['id'] in target_ids and slint_ref in (p.get('license') or '')
)
if not packages:
    raise SystemExit('no Slint royalty-free packages found in target dependency graph')

lines = [
    'CUSTOM THIRD-PARTY LICENSES',
    '===========================',
    '',
    'Slint is used under the Slint Royalty-free Desktop, Mobile, and Web',
    'Applications License 2.0. Patch is a desktop application for general-',
    'purpose computers. Slint\'s attribution badge is shown on the public',
    'page Patch is downloaded from (github.com/novuon/patch-releases), as',
    'section 2(b) allows. The full license text follows.',
    '',
    'Slint packages in this target dependency graph:',
]
lines.extend(f'  {name} {version}' for name, version in packages)
lines += ['', Path('licenses/Slint-Royalty-free-2.0.md').read_text(), '']
for filename, heading in [
    ('crates/patch-engine/vendor/signalsmith/LICENSE-stretch.txt', 'Signalsmith Stretch'),
    ('crates/patch-engine/vendor/signalsmith/LICENSE-linear.txt', 'Signalsmith Linear'),
]:
    lines += [f'{heading} (MIT)', '-' * len(f'{heading} (MIT)'), '', Path(filename).read_text(), '']
output_path.write_text('\n'.join(lines))
