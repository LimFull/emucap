"""Native package boundaries: preserve relocatable links; reject path escape and ambiguity."""
import tempfile
from pathlib import Path
import unittest
import subprocess
import json
import tarfile
import plistlib
import sys
import shutil
from contextlib import ExitStack
from unittest.mock import patch
import package_native
from package_native import copy_dolphin_gui, copy_pcsx2_qt_plugins, files_manifest, one, format_of, copy_library, move_app_metadata, refresh_executable_metadata, macho_dependencies, source_archive, restore_package, sha, select_outputs, linux_dependencies


class PackageBoundaries(unittest.TestCase):
    def test_fresh_package_has_no_reuse_parent(self):
        with tempfile.TemporaryDirectory() as d, ExitStack() as mocks:
            root = Path(d).resolve()
            src, output = root / 'native', root / 'artifacts'
            src.mkdir()
            binary = src / 'emulator'
            binary.write_bytes(b'native binary fixture')
            (src / 'emucap-build.json').write_text('{}')
            (root / 'Cargo.toml').write_text('[package]\nversion="1.1.0"\n')
            (root / 'NOTICE').write_text('notice')
            def archive(*args):
                result = output / 'fixture-source.tar.gz'
                result.write_bytes(b'source fixture')
                return result
            mocks.enter_context(patch.object(sys, 'argv', ['package_native', '--source', str(root),
                '--adapter', 'np2kai', '--platform', 'linux', '--output', str(output)]))
            mocks.enter_context(patch.object(package_native, 'select_outputs', return_value=(src, src, binary, binary)))
            mocks.enter_context(patch.object(package_native, 'run', return_value='a' * 40))
            mocks.enter_context(patch.object(package_native, 'linux_dependencies', return_value=[]))
            mocks.enter_context(patch.object(package_native, 'refresh_executable_metadata'))
            mocks.enter_context(patch.object(package_native, 'source_archive', side_effect=archive))
            record = mocks.enter_context(patch('native_provenance.create_record'))
            package_native.main()
            self.assertEqual(record.call_args.kwargs['parents'], [])
            self.assertTrue(list(output.glob('emucap-*.tar.gz')))

    def test_dolphin_gui_materializes_verified_source_license_links(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve()
            src, stage = root / 'dolphin', root / 'package'
            gui = src / 'build-emucap-gui/Binaries'
            gui.mkdir(parents=True)
            stage.mkdir()
            (gui / 'dolphin-emu').write_bytes(b'executable')
            (src / 'COPYING').write_bytes(b'license')
            (src / 'LICENSES').mkdir()
            (src / 'LICENSES/MIT.txt').write_bytes(b'mit')
            (gui / 'COPYING').symlink_to(src / 'COPYING')
            (gui / 'Licenses').symlink_to(src / 'LICENSES', target_is_directory=True)

            copy_dolphin_gui(src, stage)

            self.assertFalse((stage / 'gui/COPYING').is_symlink())
            self.assertFalse((stage / 'gui/Licenses').is_symlink())
            self.assertEqual((stage / 'gui/COPYING').read_bytes(), b'license')
            self.assertEqual((stage / 'gui/Licenses/MIT.txt').read_bytes(), b'mit')
            files_manifest(stage)

    def test_dolphin_gui_rejects_license_link_outside_verified_source(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve()
            src, stage = root / 'dolphin', root / 'package'
            gui = src / 'build-emucap-gui/Binaries'
            gui.mkdir(parents=True)
            stage.mkdir()
            outside = root / 'outside-license'
            outside.write_bytes(b'host data')
            (gui / 'COPYING').symlink_to(outside)
            with self.assertRaisesRegex(ValueError, 'escapes its verified source tree'):
                copy_dolphin_gui(src, stage)

    def test_internal_link_and_content_identity(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / 'library').write_bytes(b'first')
            (root / 'alias').symlink_to('library')
            before = files_manifest(root)
            self.assertEqual(before['alias'], {'symlink': 'library'})
            (root / 'library').write_bytes(b'second')
            self.assertNotEqual(before['library'], files_manifest(root)['library'])

    def test_external_link_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / 'payload'
            root.mkdir()
            (Path(d) / 'private').write_bytes(b'outside')
            (root / 'escape').symlink_to('../private')
            with self.assertRaisesRegex(ValueError, 'escaping package symlink'):
                files_manifest(root)

    def test_library_alias_resolves_one_binary(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            ((root / 'lib.so.1').resolve()).write_bytes(b'ELF')
            (root / 'lib.so').symlink_to('lib.so.1')
            self.assertEqual(one(root, 'lib.so*').resolve(), (root / 'lib.so.1').resolve())
            (root / 'lib.so.other').write_bytes(b'different')
            with self.assertRaisesRegex(ValueError, 'expected one'):
                one(root, 'lib.so*')

    def test_shared_dependency_reuses_original_identity_after_relocation(self):
        with tempfile.TemporaryDirectory() as d:
            source, target = Path(d) / 'source', Path(d) / 'target'
            source.write_bytes(b'original library')
            origins = {}
            self.assertTrue(copy_library(source, target, origins))
            target.write_bytes(b'relocated library')
            self.assertFalse(copy_library(source, target, origins))
            source.write_bytes(b'different library')
            with self.assertRaisesRegex(ValueError, 'dependency collision'):
                copy_library(source, target, origins)

    def test_app_metadata_is_outside_resource_seal(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            metadata = root / 'Game.app/Contents/MacOS/emucap-game-build.json'
            metadata.parent.mkdir(parents=True)
            metadata.write_bytes(b'identity')
            binary = metadata.parent / 'game'
            binary.write_bytes(b'executable')
            move_app_metadata(root)
            self.assertFalse(metadata.exists())
            self.assertEqual((root / metadata.name).read_bytes(), b'identity')
            self.assertEqual(binary.read_bytes(), b'executable')
            metadata.write_bytes(b'conflicting identity')
            with self.assertRaisesRegex(ValueError, 'conflicting app metadata'):
                move_app_metadata(root)

    def test_relocated_executable_refreshes_every_identity_field(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            executable = root / 'np2kai_libretro.so'
            executable.write_bytes(b'relocated executable')
            metadata = root / 'emucap-np2kai-build.json'
            metadata.write_text(json.dumps({
                'core_sha256': '1' * 64,
                'binary_sha256': '2' * 64,
                'patchset_sha256': '3' * 64,
            }))

            refresh_executable_metadata(root, executable)

            value = json.loads(metadata.read_text())
            self.assertEqual(value['core_sha256'], sha(executable))
            self.assertEqual(value['binary_sha256'], sha(executable))
            self.assertEqual(value['patchset_sha256'], '3' * 64)

    @unittest.skipUnless(sys.platform == 'darwin', 'Mach-O loader fixture')
    def test_relocated_library_resolves_original_loader_relative_dependency(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve()
            original, stage = root / 'original', root / 'package'
            original.mkdir()
            stage.mkdir()
            (original / 'leaf.c').write_text('int leaf(void) { return 42; }')
            (original / 'parent.c').write_text('int leaf(void); int parent(void) { return leaf(); }')
            (root / 'main.c').write_text('int parent(void); int main(void) { return parent() != 42; }')
            leaf, parent = original / 'libleaf.dylib', original / 'libparent.dylib'
            subprocess.run(['cc', '-dynamiclib', str(original / 'leaf.c'), '-o', str(leaf),
                            '-Wl,-install_name,@loader_path/libleaf.dylib'], check=True)
            subprocess.run(['cc', '-dynamiclib', str(original / 'parent.c'), str(leaf),
                            '-o', str(parent), '-Wl,-install_name,' + str(parent)], check=True)
            binary = stage / 'probe'
            subprocess.run(['cc', str(root / 'main.c'), str(parent), '-o', str(binary)], check=True)
            macho_dependencies(stage, binary)
            # Removing the build directory proves the original library is no longer needed.
            original.rename(root / 'unavailable')
            subprocess.run([str(binary)], check=True)

    def test_external_source_reference_is_preserved_without_host_content(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            repo, src, output = root / 'repo', root / 'upstream', root / 'output'
            for p in (repo, src, output): p.mkdir()
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            (root / 'host-header').write_text('private host bytes')
            (src / 'system-headers').symlink_to('../host-header')
            (src / 'source.c').write_text('int main(void) { return 0; }')
            archive = source_archive(repo, src, 'xemu', output, 'example')
            with tarfile.open(archive) as packed:
                self.assertNotIn('example-source/upstream/system-headers', packed.getnames())
                data = json.load(packed.extractfile('example-source/EXTERNAL-SYMLINKS.json'))
                self.assertEqual(data, {'system-headers': '../host-header'})
                packed.extractall(root / 'restored', filter='data')
            self.assertFalse((root / 'restored/example-source/upstream/system-headers').exists())

    def test_reuse_requires_matching_revision_and_archive_bytes(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            old, inputs = root / 'old' / 'example', root / 'inputs'
            old.mkdir(parents=True)
            inputs.mkdir()
            (old / 'game').write_bytes(b'executable')
            identity = dict(source_revision='revision', adapter='example', target='test',
                            version='1.0.0', executable='game')
            (old / 'NATIVE-PACKAGE.json').write_text(json.dumps(dict(identity, files=files_manifest(old))))
            archive = inputs / 'example.tar.gz'
            with tarfile.open(archive, 'w:gz') as packed: packed.add(old, arcname='example')
            sources = inputs / 'example-source.tar.gz'
            sources.write_bytes(b'preserved upstream source archive')
            record = dict(identity, artifacts={p.name: sha(p) for p in (archive, sources)})
            (inputs / 'example.json').write_text(json.dumps(record))
            stage = root / 'restored' / 'example'
            self.assertEqual(restore_package(inputs, 'example', 'revision', stage).read_bytes(), b'executable')
            with self.assertRaisesRegex(ValueError, 'producer revision mismatch'):
                restore_package(inputs, 'example', 'other', root / 'wrong' / 'example')
            archive.write_bytes(b'tampered')
            with self.assertRaisesRegex(ValueError, 'digest mismatch'):
                restore_package(inputs, 'example', 'revision', root / 'tampered' / 'example')

    def test_pcsx2_outputs_follow_recipe_layout(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build = root / 'adapters/pcsx2/work/pcsx2/build-emucap'
            app = build / 'pcsx2-qt/PCSX2.app'
            binary = app / 'Contents/MacOS/PCSX2'
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b'native')
            (app / 'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleExecutable': 'PCSX2'}))
            self.assertEqual(select_outputs(root, 'pcsx2', True)[2:], (app, binary))
            linux = build / 'pcsx2-qt/pcsx2-qt'
            linux.write_bytes(b'native')
            self.assertEqual(select_outputs(root, 'pcsx2', False)[2:], (linux.parent, linux))

    def test_pcsx2_linux_package_carries_producer_qt_plugins(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            plugins = root / 'native-dependencies/pcsx2/plugins'
            (plugins / 'platforms').mkdir(parents=True)
            (plugins / 'platforms/libqxcb.so').write_bytes(b'xcb')
            (plugins / 'imageformats').mkdir()
            (plugins / 'imageformats/libqpng.so').write_bytes(b'png')
            executable = root / 'package/runtime/pcsx2-qt'
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b'native')

            copy_pcsx2_qt_plugins(root, executable)

            self.assertEqual((executable.parent / 'plugins/platforms/libqxcb.so').read_bytes(), b'xcb')
            self.assertEqual((executable.parent / 'plugins/imageformats/libqpng.so').read_bytes(), b'png')
            self.assertEqual((executable.parent / 'qt.conf').read_text(), '[Paths]\nPlugins = plugins\n')

    def test_pcsx2_linux_package_requires_xcb_plugin(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            executable = root / 'package/runtime/pcsx2-qt'
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b'native')
            with self.assertRaisesRegex(ValueError, 'missing PCSX2 Qt platform plugin'):
                copy_pcsx2_qt_plugins(root, executable)

    @unittest.skipUnless(sys.platform == 'linux' and shutil.which('patchelf'), 'ELF loader fixture')
    def test_linux_closure_uses_producer_libraries_for_transitive_imports(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve()
            producer, system, stage = root / 'producer', root / 'system', root / 'stage'
            for p in (producer, system, stage): p.mkdir()
            for directory, value in ((producer, 7), (system, 99)):
                code = directory / 'value.c'
                code.write_text(f'int value(void) {{ return {value}; }}')
                subprocess.run(['cc', '-shared', '-fPIC', str(code), '-Wl,-soname,libvalue.so.1',
                                '-o', str(directory / 'libvalue.so.1')], check=True)
            wrapper = system / 'wrapper.c'
            wrapper.write_text('int value(void); int wrapper(void) { return value(); }')
            subprocess.run(['cc', '-shared', '-fPIC', str(wrapper), str(system / 'libvalue.so.1'),
                            '-Wl,-soname,libwrapper.so.1', '-Wl,-rpath,' + str(system),
                            '-o', str(system / 'libwrapper.so.1')], check=True)
            main = root / 'main.c'
            main.write_text('int value(void); int wrapper(void); int main(void) { return value()!=7 || wrapper()!=7; }')
            binary = stage / 'probe'
            subprocess.run(['cc', str(main), str(producer / 'libvalue.so.1'), str(system / 'libwrapper.so.1'),
                            '-Wl,-rpath,' + str(producer), '-Wl,-rpath,' + str(system), '-o', str(binary)], check=True)
            linux_dependencies(stage, [producer])
            producer.rename(root / 'unavailable-producer')
            system.rename(root / 'unavailable-system')
            subprocess.run([str(binary)], check=True)

    def test_file_type_uses_header(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / 'no-extension'
            path.write_bytes(b'\x7fELFpayload')
            self.assertEqual(format_of(path), 'elf')
            path.write_bytes(b'\xcf\xfa\xed\xfepayload')
            self.assertEqual(format_of(path), 'macho')
            path.write_bytes(b'ordinary resource')
            self.assertIsNone(format_of(path))


if __name__ == '__main__':
    unittest.main()
