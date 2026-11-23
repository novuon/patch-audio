#!/usr/bin/env python3
"""Reject application sources and unexpected files before a kit leaves private CI."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import struct
import tarfile

RUNTIME_FILES = {
    'VERSION', 'LICENSE', 'NOTICE.md', 'README.txt', 'install.sh', 'install-key.sh', 'uninstall.sh',
    'crates/patch-core/modules.json',
    'THIRD_PARTY_LICENSES.html', 'THIRD_PARTY_LICENSES-CUSTOM.txt',
    'bin/omarchy-patch', 'bin/patch-remote',
    'packaging/70-omarchy-patch-controllers.rules',
    'tools/stems/setup.py', 'tools/stems/demucs_adapter.py',
    'remote/patch-remote-launch', 'remote/patch-remote.service',
    'remote/patch-remote-follow.path', 'remote/patch-remote-follow.service',
}
MACHINES = {'x86_64': 62, 'aarch64': 183}
THEME_FILES = {
    'LICENSE', 'README.md', 'alacritty.toml', 'btop.theme', 'chromium.theme',
    'claude.json', 'colors.toml', 'ghostty.conf', 'hyprland.lua', 'icons.theme',
    'kitty.conf', 'patch.toml', 'preview-unlock.png', 'preview.png',
    'shell.launcher.toml', 'shell.menu.toml', 'unlock.png',
}


def theme_resource(rel):
    parts = PurePosixPath(rel).parts
    if len(parts) < 3 or parts[0] != 'themes' or parts[1] not in ('patch-light', 'patch-dark'):
        return False
    tail = '/'.join(parts[2:])
    return tail in THEME_FILES or bool(re.fullmatch(r'backgrounds/[^/]+\.(png|jpe?g|webp)', tail))


def verify(path, version, arch):
    """Return a public manifest; raise ValueError on unsafe/incomplete archives."""
    root = f'patch-{version}'
    seen = set()
    with tarfile.open(path, 'r:gz') as archive:
        for member in archive:
            name = member.name.rstrip('/')
            parts = PurePosixPath(name).parts
            if (not parts or parts[0] != root or '\\' in name
                    or name.startswith('/') or '..' in parts or '.' in name.split('/')):
                raise ValueError(f'unsafe archive path: {name}')
            if name in seen:
                raise ValueError(f'duplicate archive path: {name}')
            seen.add(name)
            if not (member.isdir() or member.isfile()):
                raise ValueError(f'links and special files are not allowed: {name}')
            rel = '/'.join(parts[1:])
            if member.isdir():
                # Directories still need to belong to the runtime allowlist.
                prefix = rel + '/' if rel else ''
                if rel and not (any(p.startswith(prefix) for p in RUNTIME_FILES)
                                or rel in ('themes', 'themes/patch-light', 'themes/patch-dark', 'integration', 'integration/desktop')
                                or rel.startswith(('themes/patch-light/', 'themes/patch-dark/'))):
                    raise ValueError(f'unexpected directory: {name}')
                continue
            if any(p in ('.git', '.github', 'node_modules') for p in parts) or rel.lower().endswith(('.rs', '.slint', '.c', '.cc', '.cpp', '.h', '.hh', '.hpp', '.s', '.asm', '.qml')) or PurePosixPath(rel).name in ('Cargo.toml', 'Cargo.lock'):
                raise ValueError(f'application source/build content: {name}')
            allowed = rel in RUNTIME_FILES
            allowed |= theme_resource(rel)
            allowed |= rel.startswith('integration/desktop/') and rel.endswith(('.desktop', '.svg'))
            if not allowed:
                raise ValueError(f'file outside runtime allowlist: {name}')
            stream = archive.extractfile(member)
            if rel == 'VERSION' and stream.read().decode().strip() != version:
                raise ValueError('VERSION does not match the requested version')
            if rel == 'THIRD_PARTY_LICENSES.html':
                text = stream.read().decode(errors='replace')
                if 'Third Party Licenses' not in text or 'MIT License' not in text:
                    raise ValueError('generated Rust third-party license report is incomplete')
            if rel == 'THIRD_PARTY_LICENSES-CUSTOM.txt':
                text = stream.read().decode(errors='replace')
                required = ('Slint Royalty-free Desktop, Mobile, and Web',
                            'Signalsmith Stretch (MIT)', 'Signalsmith Linear (MIT)')
                if any(part not in text for part in required):
                    raise ValueError('custom third-party license notices are incomplete')
            if rel in ('bin/omarchy-patch', 'bin/patch-remote'):
                header = stream.read(20)
                if (len(header) != 20 or header[:6] != b'\x7fELF\x02\x01'
                        or struct.unpack('<H', header[18:20])[0] != MACHINES[arch]
                        or not member.mode & 0o111):
                    raise ValueError(f'not an executable 64-bit {arch} ELF: {name}')
    files = {name[len(root) + 1:] for name in seen if name != root}
    missing = RUNTIME_FILES - files
    if missing:
        raise ValueError('missing required runtime files: ' + ', '.join(sorted(missing)))
    if not any(p.startswith('integration/desktop/') and p.endswith('.desktop') for p in files):
        raise ValueError('desktop launchers missing')
    # A release kit (KIT_THEMES=0) has no themes: they download as a module.
    # A test kit has both or neither.
    themed = [t for t in ('patch-light', 'patch-dark')
              if any(p.startswith(f'themes/{t}/') for p in files)]
    if len(themed) == 1:
        raise ValueError('only one theme in the kit: ' + themed[0])
    return {'archive': Path(path).name, 'sha256': hashlib.sha256(Path(path).read_bytes()).hexdigest(),
            'version': version, 'architecture': arch, 'runtime_entries': len(files)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--version', required=True)
    parser.add_argument('--arch', required=True, choices=MACHINES)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--metadata', required=True, type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}', args.commit):
        parser.error('expected a full source commit hash')
    manifest = verify(args.archive, args.version, args.arch)
    manifest.update(tag=args.tag, source_commit=args.commit)
    args.metadata.write_text(json.dumps(manifest, indent=2) + '\n')
    print(f'Verified runtime-only kit: {args.archive.name}')


if __name__ == '__main__':
    main()
