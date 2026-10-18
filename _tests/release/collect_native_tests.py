import tempfile
import json
import subprocess
import tarfile
from pathlib import Path
import unittest
from collect_native import ADAPTERS, PREBUILT_PLATFORMS, validate_selection, merge_file, validate_package
from package_native import source_archive, files_manifest, sha


class CollectionTests(unittest.TestCase):
    def test_missing_or_duplicate_platform_refuses_release(self):
        selection = [dict(adapter=a, platform=p, run=1) for a in ADAPTERS for p in PREBUILT_PLATFORMS[a]]
        self.assertEqual(len(selection), 39)
        validate_selection(selection)
        without_linux_pcsx2 = [r for r in selection if (r['adapter'], r['platform']) != ('pcsx2', 'linux')]
        with self.assertRaisesRegex(ValueError, 'every adapter'):
            validate_selection(without_linux_pcsx2)
        with self.assertRaisesRegex(ValueError, 'every adapter'): validate_selection(selection[:-1])
        with self.assertRaisesRegex(ValueError, 'every adapter'): validate_selection(selection + [selection[0]])

    def test_release_rejects_stale_native_code_recipes_and_shared_inputs(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            source, upstream, output = root / 'source', root / 'upstream', root / 'output'
            for p in (source, upstream, output): p.mkdir()
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            (source / 'Cargo.toml').write_text('[package]\nversion="1.0.0"\n')
            native = source / 'adapters/example/emucap.cpp'
            native.parent.mkdir(parents=True)
            native.write_text('native source')
            inputs = [native, native.with_name('build.sh'), native.with_name('build.ps1'),
                      source / 'adapters/_common/endpoint.h']
            for path in inputs[1:]:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('original build input')
            subprocess.run(['git', '-C', str(source), 'add', '.'], check=True)
            name = 'emucap-1.0.0-example-x86_64-unknown-linux-gnu'
            stage = root / name; stage.mkdir()
            (stage / 'game').write_bytes(b'compiled game')
            record = dict(adapter='example', target='x86_64-unknown-linux-gnu', version='1.0.0',
                          source_revision='original', executable='game')
            (stage / 'NATIVE-PACKAGE.json').write_text(json.dumps(dict(record, files=files_manifest(stage))))
            runtime = output / (name + '.tar.gz')
            with tarfile.open(runtime, 'w:gz') as archive: archive.add(stage, arcname=name)
            sources = source_archive(source, upstream, 'example', output, name)
            record['artifacts'] = {p.name: sha(p) for p in (runtime, sources)}
            (output / (name + '.json')).write_text(json.dumps(record))
            validate_package(output, source, 'example', 'linux')
            for path in inputs:
                original = path.read_bytes()
                path.write_bytes(b'changed build input')
                with self.subTest(path=path.relative_to(source)), \
                        self.assertRaisesRegex(ValueError, 'incompatible with release source'):
                    validate_package(output, source, 'example', 'linux')
                path.write_bytes(original)
            validate_package(output, source, 'example', 'linux')

    def test_shared_source_must_have_identical_bytes(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            output = root / 'out'; output.mkdir()
            source = root / 'source.tar.gz'
            source.write_bytes(b'original')
            merge_file(source, output)
            source.write_bytes(b'original')
            merge_file(source, output)
            source.write_bytes(b'conflicting')
            with self.assertRaisesRegex(ValueError, 'conflicting release artifact'): merge_file(source, output)
            self.assertEqual((output / source.name).read_bytes(), b'original')


if __name__ == '__main__': unittest.main()
