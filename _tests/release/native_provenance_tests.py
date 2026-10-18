"""Exercise producer authority, recipe equality, and authenticated reuse boundaries."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import native_payload as payload
import native_producer as producer
import native_provenance as provenance

REPO = 'owner/emucap'
REVISION = 'a' * 40


def identity(purpose='package', adapter='ppsspp', platform='linux', run=12):
    workflow, job, _ = producer.producer_spec(purpose, adapter, platform)
    return dict(repository=REPO, repository_id=10, workflow='.github/workflows/' + workflow,
                revision=REVISION, ref='refs/heads/main', run=run, attempt=2,
                event='workflow_dispatch', job=job)


def producer_fixture():
    repository = dict(id=10, full_name=REPO)
    workflow = dict(id=3, path='.github/workflows/native-unix.yml')
    info = dict(id=12, run_attempt=2, status='completed', conclusion='failure', event='workflow_dispatch',
                repository=repository.copy(), head_repository=repository.copy(), head_branch='main',
                head_sha=REVISION, workflow_id=3, path=workflow['path'])
    jobs = [dict(name='build (ppsspp, linux)', status='completed', conclusion='success',
                 run_id=12, run_attempt=2, head_sha=REVISION),
            dict(name='build (dolphin, linux)', status='completed', conclusion='failure')]
    return info, jobs, repository, workflow


class ProducerTests(unittest.TestCase):
    def test_toolchain_requires_hosted_image_or_digest_pinned_container(self):
        base = dict(RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='Linux', RUNNER_ARCH='X64')
        for values, accepted in [({'ImageVersion': '20261016.1'}, True),
                                 ({'NATIVE_CONTAINER': 'toolchain@sha256:' + 'a' * 64}, True),
                                 ({'NATIVE_CONTAINER': 'toolchain:latest'}, False),
                                 ({}, False),
                                 ({'ImageVersion': 'image', 'RUNNER_ENVIRONMENT': 'self-hosted'}, False)]:
            with self.subTest(values=values), patch.dict(provenance.os.environ, dict(base, **values), clear=True), \
                    patch('native_provenance.shutil.which', return_value=None):
                if accepted:
                    result = provenance.toolchain_identity()
                    self.assertEqual(result['os'], 'Linux')
                    self.assertTrue(result.get('image') or result.get('container'))
                else:
                    with self.assertRaises(ValueError): provenance.toolchain_identity()

    def test_successful_matrix_job_survives_sibling_failure(self):
        args = producer_fixture()
        self.assertEqual(producer.admit_run(*args, 'package', 'ppsspp', 'linux'), identity())

    def test_wrong_workflow_event_repository_ref_and_status_are_rejected(self):
        mutations = [('status', 'in_progress'), ('event', 'pull_request'), ('event', 'pull_request_target'),
                     ('event', 'workflow_run'), ('head_branch', 'unreviewed'), ('head_sha', 'bad'),
                     ('workflow_id', 4), ('path', '.github/workflows/other.yml'),
                     ('repository', dict(id=11, full_name=REPO)),
                     ('head_repository', dict(id=11, full_name=REPO))]
        for key, value in mutations:
            with self.subTest(key=key, value=value):
                args = producer_fixture()
                args[0][key] = value
                with self.assertRaisesRegex(ValueError, 'approved producer'):
                    producer.admit_run(*args, 'package', 'ppsspp', 'linux')

    def test_exact_job_and_attempt_are_required(self):
        for key, value in [('conclusion', 'failure'), ('status', 'queued'), ('run_attempt', 1),
                           ('run_id', 13), ('head_sha', 'b' * 40)]:
            args = producer_fixture()
            args[1][0][key] = value
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'exact native producer job'):
                producer.admit_run(*args, 'package', 'ppsspp', 'linux')
        for jobs in ([], [producer_fixture()[1][0]] * 2):
            args = producer_fixture()
            with self.assertRaisesRegex(ValueError, 'exact native producer job'):
                producer.admit_run(args[0], jobs, *args[2:], 'package', 'ppsspp', 'linux')

    def test_producer_failure_precedes_payload_download(self):
        with patch.object(provenance, 'resolve_producer', side_effect=ValueError('wrong workflow')), \
                patch.object(provenance, 'download_payload') as download:
            with self.assertRaisesRegex(ValueError, 'wrong workflow'):
                provenance.fetch(REPO, 12, 'package', 'ppsspp', 'linux', Path('unused'))
        download.assert_not_called()

    def test_certificate_invocation_is_required_not_a_predicate_claim(self):
        good = {'verificationResult': {'signature': {'certificate': {
            'runInvocationURI': f'https://github.com/{REPO}/actions/runs/12/attempts/2',
            'sourceRepositoryIdentifier': '10', 'buildTrigger': 'workflow_dispatch'}}}}
        provenance.check_signature_result([good], identity())
        for key in ('runInvocationURI', 'sourceRepositoryIdentifier', 'buildTrigger'):
            bad = copy.deepcopy(good)
            bad['verificationResult']['signature']['certificate'][key] = 'wrong'
            bad['verificationResult']['statement'] = {'predicate': good}
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'certificate'):
                provenance.check_signature_result([bad], identity())
        with patch.object(provenance, 'command', return_value=json.dumps([good])) as command:
            provenance.verify_signature(Path('record'), Path('signature'), identity())
        args = command.call_args.args
        for flag, value in [('--repo', REPO), ('--signer-workflow', REPO + '/.github/workflows/native-unix.yml'),
                            ('--signer-digest', REVISION), ('--source-digest', REVISION),
                            ('--source-ref', 'refs/heads/main')]:
            self.assertEqual(args[args.index(flag) + 1], value)
        self.assertIn('--deny-self-hosted-runners', args)


class InputTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        names = ['adapters/ppsspp/upstream.lock', 'adapters/ppsspp/build.sh',
                 'adapters/ppsspp/build.ps1', 'adapters/ppsspp/native.cpp',
                 'adapters/ppsspp/README.md', 'adapters/_common/endpoint.h',
                 'src/broker.rs', 'Cargo.toml', '_tests/adapters/build-lock-test.sh',
                 '_tests/native/np2kai-load-api.py', '_tests/release/native_unix_build.sh',
                 '_tests/release/native_build.py', '_tests/release/native_producer.py',
                 '_tests/release/native_provenance.py', '_tests/release/native_payload.py']
        names += ['.github/workflows/' + name for name in ('native-unix.yml', 'native-adapters.yml',
                  'native-msys2.yml', 'native-xemu-windows.yml')]
        for name in names:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('original')
        self.track()

    def track(self):
        subprocess.run(['git', '-C', str(self.root), 'add', '-A'], check=True)

    def native(self, platform='linux'):
        return producer.native_inputs(self.root, self.root, 'ppsspp', platform)

    def test_complete_native_recipes_include_shell_powershell_common_and_ci(self):
        before = self.native()
        for name in ['adapters/ppsspp/build.sh', 'adapters/ppsspp/build.ps1',
                     'adapters/_common/endpoint.h', '_tests/release/native_unix_build.sh']:
            path = self.root / name
            path.write_text('changed')
            with self.subTest(name=name): self.assertNotEqual(before, self.native())
            path.write_text('original')
        extra = self.root / 'adapters/ppsspp/added.h'
        extra.write_text('added')
        self.track()
        self.assertNotEqual(before, self.native())
        extra.unlink()
        self.track()
        self.assertEqual(before, self.native())
        (self.root / 'adapters/ppsspp/native.cpp').unlink()
        self.track()
        self.assertNotEqual(before, self.native())

    def test_core_version_docs_and_unused_msvc_common_do_not_invalidate_native(self):
        unix, windows = self.native(), self.native('windows')
        for name in ('src/broker.rs', 'Cargo.toml', 'adapters/ppsspp/README.md'):
            (self.root / name).write_text('changed')
        self.assertEqual(unix, self.native())
        (self.root / 'adapters/_common/endpoint.h').write_text('changed')
        self.assertEqual(windows, self.native('windows'))
        self.assertNotEqual(unix, self.native())

    def test_pipeline_gate_rejects_old_or_incomplete_helper_trees(self):
        expected = producer.pipeline_blobs(self.root)
        tree = dict(truncated=False, tree=[dict(path=name, sha=digest, mode='100644', type='blob')
                                           for name, digest in expected.items()])
        with patch.object(producer, 'github', return_value=tree):
            producer.admit_pipeline({'head_sha': REVISION}, self.root, REPO)
        for mutation in ('change', 'remove', 'extra', 'symlink', 'truncated'):
            bad = copy.deepcopy(tree)
            if mutation == 'change': bad['tree'][0]['sha'] = 'b' * 40
            elif mutation == 'remove': bad['tree'].pop(0)
            elif mutation == 'extra': bad['tree'].append(dict(path='_tests/release/extra.py', sha='b' * 40, mode='100644', type='blob'))
            elif mutation == 'symlink': bad['tree'][0]['mode'] = '120000'
            else: bad['truncated'] = True
            with self.subTest(mutation=mutation), patch.object(producer, 'github', return_value=bad), self.assertRaises(ValueError):
                producer.admit_pipeline({'head_sha': REVISION}, self.root, REPO)

    def test_reviewed_pipeline_requires_exact_repository_revision_and_input_map(self):
        previous = producer.pipeline_blobs(self.root)
        previous['_tests/release/native_producer.py'] = 'b' * 40
        tree = dict(truncated=False, tree=[dict(path=name, sha=digest, mode='100644', type='blob')
                                           for name, digest in previous.items()])
        approvals = {(REPO, REVISION): producer.pipeline_digest(previous)}
        with patch.object(producer, 'APPROVED_PIPELINES', approvals), patch.object(producer, 'github', return_value=tree):
            producer.admit_pipeline({'head_sha': REVISION}, self.root, REPO)
            for repo, revision in [('other/emucap', REVISION), (REPO, 'c' * 40)]:
                with self.assertRaises(ValueError):
                    producer.admit_pipeline({'head_sha': revision}, self.root, repo)
        for mutation in ('change', 'remove', 'extra', 'symlink', 'truncated'):
            bad = copy.deepcopy(tree)
            if mutation == 'change': bad['tree'][0]['sha'] = 'c' * 40
            elif mutation == 'remove': bad['tree'].pop(0)
            elif mutation == 'extra': bad['tree'].append(dict(path='_tests/release/extra.py', sha='c' * 40, mode='100644', type='blob'))
            elif mutation == 'symlink': bad['tree'][0]['mode'] = '120000'
            else: bad['truncated'] = True
            with self.subTest(mutation=mutation), patch.object(producer, 'APPROVED_PIPELINES', approvals), patch.object(producer, 'github', return_value=bad), self.assertRaises(ValueError):
                producer.admit_pipeline({'head_sha': REVISION}, self.root, REPO)

    def test_private_workflow_overlay_has_public_identity(self):
        before = producer.pipeline_blobs(self.root)
        public = self.root / '.github/workflows/native-unix.yml'
        overlay = self.root / 'tools/deploy/public/.github/workflows/native-unix.yml'
        overlay.parent.mkdir(parents=True)
        public.rename(overlay)
        self.track()
        self.assertEqual(before, producer.pipeline_blobs(self.root))


def write_record(root, run=12, parents=(), native=None):
    root.mkdir()
    (root / 'runtime.bin').write_bytes(b'payload')
    provenance.retain_parents(root, parents)
    native = native or dict(source_revision='b' * 40, inputs={'source': {'native.cpp': 'c' * 64}},
                            toolchain={'image': 'original-image'})
    record = dict(purpose='package', adapter='ppsspp', platform='linux', producer=identity(run=run),
                  native=native, files=payload.payload_files(root), parents=[p.digest for p in parents],
                  release_revision=None)
    (root / payload.RECORD).write_bytes(provenance.canonical(record))
    (root / payload.SIGNATURE).write_bytes(b'synthetic signature; verification separately mocked')
    return record


class LineageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        resolver = patch.object(provenance, 'resolve_producer', side_effect=lambda repo, run, attempt, purpose, adapter, platform, authority:
                                identity(purpose, adapter, platform, run))
        signature = patch.object(provenance, 'verify_signature')
        resolver.start(); signature.start()
        self.addCleanup(resolver.stop); self.addCleanup(signature.stop)

    def verify(self, path):
        return provenance.Verifier(REPO).verify(path, 'package', 'ppsspp', 'linux')

    def test_dual_checkout_and_authenticated_repackaging_preserve_native_identity(self):
        original = write_record(self.root / 'original')
        first = self.verify(self.root / 'original')
        write_record(self.root / 'repacked', run=13, parents=[first], native=original['native'])
        repacked = self.verify(self.root / 'repacked')
        self.assertNotEqual(repacked.record['native']['source_revision'], repacked.record['producer']['revision'])
        self.assertEqual(repacked.record['native'], first.record['native'])
        self.assertEqual(set(repacked.proofs), {repacked.digest, first.digest})

    def test_mutated_or_unsigned_parent_cannot_be_laundered(self):
        write_record(self.root / 'original')
        first = self.verify(self.root / 'original')
        for mutation in ('record', 'signature', 'missing'):
            target = self.root / mutation
            write_record(target, run=13, parents=[first], native=first.record['native'])
            record, signature = provenance.parent_paths(target / payload.PARENTS, first.digest)
            if mutation == 'record': record.write_bytes(b'{}')
            elif mutation == 'signature': signature.write_bytes(b'changed')
            else: signature.unlink()
            with self.subTest(mutation=mutation), self.assertRaises((ValueError, FileNotFoundError)):
                self.verify(target)

    def test_repackaging_cannot_replace_original_toolchain_or_source(self):
        write_record(self.root / 'original')
        first = self.verify(self.root / 'original')
        native = copy.deepcopy(first.record['native'])
        native['toolchain'] = {'image': 'new-image'}
        write_record(self.root / 'repacked', run=13, parents=[first], native=native)
        with self.assertRaisesRegex(ValueError, 'original native identity'):
            self.verify(self.root / 'repacked')

    def test_runtime_cache_retains_authenticated_birth_revision(self):
        native = dict(source_revision=REVISION, inputs={'source': {'x': 'b' * 64}},
                      toolchain={'image': 'original-image'})
        parent = dict(purpose='runtime', adapter='ppsspp', platform='windows',
                      native=native, producer=identity('runtime', 'ppsspp', 'windows'), release_revision=None)
        current = copy.deepcopy(parent)
        current['producer']['revision'] = 'c' * 40
        provenance.Verifier.validate_lineage(current, [parent])
        with self.assertRaisesRegex(ValueError, 'own native checkout'):
            provenance.Verifier.validate_lineage(current, [])
        current['native']['toolchain'] = {'image': 'current-image'}
        with self.assertRaisesRegex(ValueError, 'authenticated native origin'):
            provenance.Verifier.validate_lineage(current, [parent])

    def test_extra_or_modified_payload_and_duplicate_json_keys_rejected(self):
        for mutation in ('extra', 'modified', 'empty-directory', 'duplicate-json'):
            path = self.root / mutation
            write_record(path)
            if mutation == 'extra': (path / 'extra.bin').write_bytes(b'extra')
            elif mutation == 'modified': (path / 'runtime.bin').write_bytes(b'other')
            elif mutation == 'empty-directory': (path / 'extra').mkdir()
            else:
                original = (path / payload.RECORD).read_bytes()
                (path / payload.RECORD).write_bytes(b'{"purpose":"package",' + original[1:])
            with self.subTest(mutation=mutation), self.assertRaises(ValueError): self.verify(path)

    def test_signature_failure_precedes_payload_reads(self):
        path = self.root / 'unsigned'
        write_record(path)
        with patch.object(provenance, 'verify_signature', side_effect=ValueError('invalid signature')), \
                patch.object(provenance, 'payload_files') as reads:
            with self.assertRaisesRegex(ValueError, 'invalid signature'): self.verify(path)
        reads.assert_not_called()

    def test_release_requires_the_complete_native_input_map(self):
        path = self.root / 'incomplete-inputs'
        write_record(path)
        with patch.object(provenance, 'native_inputs', return_value={'source': {'native.cpp': 'c' * 64, 'build.sh': 'd' * 64}}):
            with self.assertRaisesRegex(ValueError, 'inputs differ'):
                provenance.Verifier(REPO).verify(path, 'package', 'ppsspp', 'linux', Path('release-source'))


class PayloadTests(unittest.TestCase):
    def test_immutable_artifact_metadata_must_match_producer(self):
        artifact = dict(id=9, name='native-package-ppsspp-linux', expired=False, size_in_bytes=12,
                        digest='sha256:' + 'd' * 64, workflow_run=dict(id=12, head_sha=REVISION,
                        repository_id=10, head_repository_id=10, head_branch='main'))
        self.assertEqual(payload.select_artifact([artifact], identity(), 'package', 'ppsspp', 'linux'), artifact)
        for key, value in [('id', 0), ('digest', ''), ('size_in_bytes', 0), ('expired', True)]:
            bad = copy.deepcopy(artifact); bad[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                payload.select_artifact([bad], identity(), 'package', 'ppsspp', 'linux')
        with self.assertRaises(ValueError):
            payload.select_artifact([artifact, artifact], identity(), 'package', 'ppsspp', 'linux')

    def test_archive_traversal_aliases_links_and_budget_reject_before_extraction(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for index, names in enumerate([['../outside'], ['C:/outside'], ['a\\b'], ['x', 'X'], ['one', 'two']]):
                archive = root / f'{index}.zip'
                with zipfile.ZipFile(archive, 'w') as packed:
                    for name in names: packed.writestr(name, 'data')
                output = root / str(index)
                with patch.object(payload, 'MAX_FILES', 1 if names == ['one', 'two'] else 65536), self.assertRaises(ValueError):
                    payload.extract_download(archive, output)
                self.assertFalse(output.exists())
            archive = root / 'link.zip'
            with zipfile.ZipFile(archive, 'w') as packed:
                entry = zipfile.ZipInfo('link'); entry.external_attr = 0o120777 << 16
                packed.writestr(entry, 'target')
            with self.assertRaises(ValueError): payload.extract_download(archive, root / 'link')


if __name__ == '__main__': unittest.main()
