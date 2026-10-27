import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('legacy_snapshot', Path(__file__).parents[1] / 'recipes/legacy/snapshot.py')
snapshot = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = snapshot
spec.loader.exec_module(snapshot)


class SnapshotTests(unittest.TestCase):
    def test_selection_validation(self):
        for roots in ([], ['../secret'], ['/root'], ['work', 'work/repo'], ['work', 'work']):
            with self.assertRaises(ValueError):
                snapshot.Selection.parse({'version': 1, 'roots': roots, 'exclude': []})

    def test_roundtrip_preserves_untracked_files_modes_links_and_history(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / 'source'
            repo = root / 'work/repo'
            repo.mkdir(parents=True)
            (repo / 'untracked').write_bytes(b'pending work\0\xff')
            (repo / 'untracked').chmod(0o751)
            os.utime(repo / 'untracked', (1700000000, 1700000000))
            (repo / 'link').symlink_to('untracked')
            (repo / 'hardlink').hardlink_to(repo / 'untracked')
            (repo / 'target').mkdir()
            (repo / 'target/cache').write_text('omit')
            (repo / 'readonly').mkdir()
            (repo / 'readonly/file').write_text('retained')
            (repo / 'readonly').chmod(0o555)
            history = root / '.kuggis/chat/pi'
            history.mkdir(parents=True)
            (history / 'session.jsonl').write_text('{"type":"session"}\n')
            (root / 'auth.json').write_text('not selected')
            selection = snapshot.Selection.parse({'version': 1, 'roots': ['work', '.kuggis/chat/pi'], 'exclude': ['*/target']})
            archive = Path(tmp) / 'snapshot.tar.gz'
            with archive.open('wb') as output:
                receipt = snapshot.export(root, selection, output)
            restored = Path(tmp) / 'restored'
            result = snapshot.restore(archive, restored)
            with archive.open('rb') as stream:
                streamed = snapshot.restore(stream, Path(tmp) / 'streamed')
            self.assertEqual(streamed, result)
            self.assertEqual(receipt['manifest_sha256'], result['manifest_sha256'])
            self.assertEqual((restored / 'work/repo/untracked').read_bytes(), b'pending work\0\xff')
            self.assertEqual((restored / 'work/repo/untracked').stat().st_mode & 0o777, 0o751)
            self.assertEqual((restored / 'work/repo/untracked').stat().st_mtime, 1700000000)
            self.assertTrue((restored / 'work/repo/link').is_symlink())
            self.assertTrue((restored / '.kuggis/chat/pi/session.jsonl').is_file())
            self.assertFalse((restored / 'auth.json').exists())
            self.assertFalse((restored / 'work/repo/target').exists())
            self.assertEqual((restored / 'work/repo/readonly/file').read_text(), 'retained')
            self.assertEqual((restored / 'work/repo/readonly').stat().st_mode & 0o777, 0o555)
            with self.assertRaises(FileExistsError):
                snapshot.restore(archive, restored)

    def test_rejects_traversal_symlink_ancestors_and_missing_manifest(self):
        for names in (['../escape'], ['link', 'link/escape'], ['plain']):
            with self.subTest(names=names), tempfile.TemporaryDirectory() as tmp:
                archive = Path(tmp) / 'bad.tar.gz'
                with tarfile.open(archive, 'w:gz') as out:
                    for name in names:
                        member = tarfile.TarInfo(name)
                        if name == 'link':
                            member.type = tarfile.SYMTYPE
                            member.linkname = str(Path(tmp) / 'outside')
                        out.addfile(member, io.BytesIO(b''))
                with self.assertRaises(ValueError):
                    snapshot.restore(archive, Path(tmp) / 'destination')
                self.assertFalse((Path(tmp) / 'escape').exists())
                self.assertFalse((Path(tmp) / 'outside').exists())

    def test_manifest_mismatch_is_not_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / 'bad.tar.gz'
            with tarfile.open(archive, 'w:gz') as out:
                member = tarfile.TarInfo('file')
                member.size = 8
                out.addfile(member, io.BytesIO(b'modified'))
                manifest = json.dumps({'version': 1, 'entries': [{
                    'path': 'file', 'mode': member.mode & 0o777, 'kind': 'file',
                    'size': 8, 'sha256': snapshot.hashlib.sha256(b'original').hexdigest(),
                }]}).encode()
                member = tarfile.TarInfo(snapshot.MANIFEST)
                member.size = len(manifest)
                out.addfile(member, io.BytesIO(manifest))
            with self.assertRaises(ValueError):
                snapshot.restore(archive, Path(tmp) / 'destination')

    def test_export_refuses_a_file_changed_during_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / 'work'
            source.write_text('before')
            selection = snapshot.Selection.parse({'version': 1, 'roots': ['work'], 'exclude': []})
            original = tarfile.TarFile.addfile
            def changed(archive, member, data=None):
                original(archive, member, data)
                if member.name == 'work':
                    source.write_text('after')
            with patch.object(tarfile.TarFile, 'addfile', changed):
                with self.assertRaisesRegex(RuntimeError, 'source changed'):
                    snapshot.export(root, selection, io.BytesIO())


if __name__ == '__main__':
    unittest.main()
