#!/usr/bin/env python3
"""Exercise the publication boundary without compiling or running Patch."""
import importlib.util
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('verify_public_kit', Path(__file__).resolve().parents[1] / 'scripts/verify-public-kit.py')
kit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(kit)


class PublicationBoundary(unittest.TestCase):
    def fixture(self, directory, change=None, extra=None):
        records = {}
        for name in kit.RUNTIME_FILES:
            body = b'test runtime resource\n'
            mode = 0o644
            if name == 'VERSION':
                body = b'0.7.8\n'
            if name == 'THIRD_PARTY_LICENSES.html':
                body = b'<html><h1>Third Party Licenses</h1>MIT License</html>\n'
            if name == 'THIRD_PARTY_LICENSES-CUSTOM.txt':
                body = (b'Slint Royalty-free Desktop, Mobile, and Web\n'
                        b'Signalsmith Stretch (MIT)\nSignalsmith Linear (MIT)\n')
            if name.startswith('bin/'):
                body = b'\x7fELF\x02\x01' + b'\0' * 12 + struct.pack('<H', 62)
                mode = 0o755
            records[name] = (body, mode)
        records['integration/desktop/omarchy-patch.desktop'] = (b'[Desktop Entry]\n', 0o644)
        for theme in ['patch-dark', 'patch-light']:
            records[f'themes/{theme}/patch.toml'] = (b'name="Patch"\n', 0o644)
        if change:
            change(records)
        path = Path(directory) / 'kit.tar.gz'
        with tarfile.open(path, 'w:gz') as out:
            for name, (body, mode) in records.items():
                item = tarfile.TarInfo('patch-0.7.8/' + name)
                item.size = len(body)
                item.mode = mode
                out.addfile(item, io.BytesIO(body))
            if extra:
                out.addfile(extra, io.BytesIO(b''))
        return path

    def check(self, change=None, extra=None, error=None):
        with tempfile.TemporaryDirectory() as directory:
            path = self.fixture(directory, change, extra)
            if error:
                with self.assertRaisesRegex(ValueError, error):
                    kit.verify(path, '0.7.8', 'x86_64')
            else:
                self.assertEqual(kit.verify(path, '0.7.8', 'x86_64')['architecture'], 'x86_64')

    def test_complete_runtime_kit(self):
        self.check()

    def test_private_source_is_rejected_even_under_theme_path(self):
        self.check(lambda r: r.update({'themes/patch-dark/private.rs': (b'', 0o644)}), error='source/build')

    def test_private_theme_notes_are_rejected(self):
        self.check(lambda r: r.update({'themes/patch-dark/private-notes.md': (b'', 0o644)}), error='allowlist')

    def test_uppercase_source_extension_is_rejected(self):
        self.check(lambda r: r.update({'themes/patch-dark/PRIVATE.RS': (b'', 0o644)}), error='source/build')

    def test_full_source_archive_is_rejected(self):
        self.check(lambda r: r.update({'Cargo.toml': (b'', 0o644)}), error='source/build')

    def test_private_notes_are_rejected(self):
        self.check(lambda r: r.update({'docs/development.md': (b'', 0o644)}), error='allowlist')

    def test_incomplete_third_party_license_report_is_rejected(self):
        self.check(lambda r: r.update({'THIRD_PARTY_LICENSES.html': (b'<html/>', 0o644)}), error='report is incomplete')

    def test_missing_custom_license_terms_are_rejected(self):
        self.check(lambda r: r.update({'THIRD_PARTY_LICENSES-CUSTOM.txt': (b'notices missing', 0o644)}), error='notices are incomplete')

    def test_missing_remote_is_rejected(self):
        self.check(lambda r: r.pop('bin/patch-remote'), error='missing required')

    def test_wrong_architecture_is_rejected(self):
        self.check(lambda r: r.update({'bin/patch-remote': (b'\x7fELF\x02\x01' + b'\0' * 12 + struct.pack('<H', 183), 0o755)}), error='ELF')

    def test_wrong_version_is_rejected(self):
        self.check(lambda r: r.update({'VERSION': (b'0.7.7\n', 0o644)}), error='VERSION')

    def test_traversal_is_rejected(self):
        self.check(lambda r: r.update({'../private': (b'', 0o644)}), error='unsafe')

    def test_symlink_is_rejected(self):
        extra = tarfile.TarInfo('patch-0.7.8/themes/patch-dark/leak')
        extra.type = tarfile.SYMTYPE
        extra.linkname = '/etc/passwd'
        self.check(extra=extra, error='links')

    def test_duplicate_member_is_rejected(self):
        self.check(extra=tarfile.TarInfo('patch-0.7.8/LICENSE'), error='duplicate')


if __name__ == '__main__':
    unittest.main()
