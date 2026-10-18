"""Final native publication accepts exactly the authenticated assembly asset set."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from collect_native import ADAPTERS, PREBUILT_PLATFORMS
from native_payload import RECORD, SIGNATURE, payload_files
from native_provenance import VerifiedPayload, Verifier, canonical
from native_release import core_assets, native_assets, prepare


def facts(raw):
    return dict(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())


def assembly(root):
    root.mkdir()
    records, parents, proofs = [], [], {}
    for adapter in ADAPTERS:
        for platform in PREBUILT_PLATFORMS[adapter]:
            target = {'linux': 'x86_64-unknown-linux-gnu', 'macos': 'aarch64-apple-darwin', 'windows': 'windows-x86_64'}[platform]
            if adapter == 'pcsx2' and platform == 'macos': target = 'x86_64-apple-darwin'
            name = f'emucap-1.0.0-{adapter}-{target}'
            archive = name + ('.zip' if platform == 'windows' else '.tar.gz')
            source = name + '-source.tar.gz'
            assets = {}
            for member in (archive, source):
                raw = ('synthetic ' + member).encode()
                (root / member).write_bytes(raw)
                assets[member] = facts(raw)
            original = dict(adapter=adapter, target=target, version='1.0.0', source_revision='a' * 40,
                            packaging_revision='b' * 40, artifacts={n: f['sha256'] for n, f in assets.items()})
            raw = canonical(original)
            (root / (name + '.json')).write_bytes(raw)
            assets[name + '.json'] = facts(raw)
            parent = dict(purpose='package', adapter=adapter, platform=platform,
                          producer=dict(run=12, revision='b' * 40), native=dict(source_revision='a' * 40), files=assets)
            digest = hashlib.sha256(canonical(parent)).hexdigest()
            parents.append(digest)
            proofs[digest] = (parent, None, None)
            records.append(dict(original, platform=platform, workflow_run=12, provenance_sha256=digest))
    index = dict(core_revision='c' * 40, packages=records)
    (root / 'NATIVE-PACKAGES.json').write_bytes(canonical(index))
    files = payload_files(root)
    (root / 'NATIVE-SHA256SUMS').write_text(''.join(f'{f["sha256"]}  {n}\n' for n, f in sorted(files.items())))
    record = dict(purpose='assembly', adapter=None, platform=None, native=None,
                  parents=parents, release_revision='c' * 40, files=payload_files(root))
    (root / RECORD).write_bytes(canonical(record))
    (root / SIGNATURE).write_bytes(b'synthetic signature; verifier tested separately')
    return VerifiedPayload(root, record, 'd' * 64, proofs)


def core(root):
    root.mkdir()
    for target, suffix in [('x86_64-unknown-linux-gnu', '.tar.gz'), ('aarch64-apple-darwin', '.tar.gz'),
                            ('x86_64-pc-windows-msvc', '.zip')]:
        name = f'emucap-1.0.0-{target}'
        raw = b'core archive fixture'
        (root / (name + suffix)).write_bytes(raw)
        (root / (name + '.sha256')).write_text(f'{hashlib.sha256(raw).hexdigest()}  {name}{suffix}\n')
        (root / (name + '.json')).write_bytes(canonical(dict(version='1.0.0', target=target, source_revision='c' * 40)))


class ReleaseTests(unittest.TestCase):
    def test_repreparation_preserves_bytes_for_partial_upload_resume(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            verified = assembly(root / 'native')
            core(root / 'core')
            with patch('gzip.time.time', return_value=1000):
                first = prepare(verified, root / 'core', root / 'first', '1.0.0')
            with patch('gzip.time.time', return_value=2000):
                second = prepare(verified, root / 'core', root / 'second', '1.0.0')
            self.assertEqual(first, second)
            for name in first:
                self.assertEqual((root / 'first' / name).read_bytes(),
                                 (root / 'second' / name).read_bytes(), name)

    def test_complete_matrix_prepares_exact_assets_and_retained_proof_archive(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            verified = assembly(root / 'native')
            Verifier.validate_lineage(verified.record, [v[0] for v in verified.proofs.values()])
            core(root / 'core')
            names = prepare(verified, root / 'core', root / 'prepared', '1.0.0')
            self.assertEqual(set(names), {p.name for p in (root / 'prepared').iterdir()})
            self.assertIn('NATIVE-PROVENANCE.tar.gz', names)
            self.assertIn('SHA256SUMS', names)
            self.assertEqual(len(names), len(native_assets(verified)) + 6 + 2)

    def test_self_consistent_extra_native_asset_is_not_published(self):
        with tempfile.TemporaryDirectory() as tmp:
            verified = assembly(Path(tmp) / 'native')
            (verified.directory / 'extra.zip').write_bytes(b'extra')
            verified.record['files']['extra.zip'] = facts(b'extra')
            with self.assertRaisesRegex(ValueError, 'undeclared release assets'):
                native_assets(verified)

    def test_index_cannot_select_another_parent_or_producer(self):
        with tempfile.TemporaryDirectory() as tmp:
            verified = assembly(Path(tmp) / 'native')
            index_path = verified.directory / 'NATIVE-PACKAGES.json'
            original = json.loads(index_path.read_text())
            for key, value in [('provenance_sha256', '0' * 64), ('workflow_run', 99),
                               ('source_revision', 'e' * 40), ('packaging_revision', 'e' * 40)]:
                index = copy.deepcopy(original)
                index['packages'][0][key] = value
                index_path.write_bytes(canonical(index))
                with self.subTest(key=key), self.assertRaises(ValueError): native_assets(verified)

    def test_extra_core_asset_and_wrong_core_revision_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            core(root / 'core')
            core_assets(root / 'core', '1.0.0', 'c' * 40)
            with self.assertRaisesRegex(ValueError, 'metadata identity'):
                core_assets(root / 'core', '1.0.0', 'd' * 40)
            (root / 'core' / 'extra.zip').write_bytes(b'extra')
            with self.assertRaisesRegex(ValueError, 'unexpected core release asset'):
                core_assets(root / 'core', '1.0.0', 'c' * 40)

    def test_native_bytes_changed_after_verification_cannot_be_published(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            verified = assembly(root / 'native')
            core(root / 'core')
            name = next(n for n in verified.record['files'] if n.endswith('.zip'))
            (verified.directory / name).write_bytes(b'changed after admission')
            with self.assertRaisesRegex(ValueError, 'changed during release preparation'):
                prepare(verified, root / 'core', root / 'output', '1.0.0')


if __name__ == '__main__': unittest.main()
