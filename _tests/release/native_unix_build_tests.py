"""Exercise the Unix recipe's consumed dependency boundary without compiling emulators."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


RECIPE = Path(__file__).with_name('native_unix_build.sh')


def write(root, name, content, executable=False):
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    if executable: path.chmod(0o755)
    return path


class UnixDependencyTests(unittest.TestCase):
    def test_linux_mesen_installs_upstream_zip_tool(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root, 'bin/uname', '#!/bin/sh\necho Linux\n', True)
            write(root, 'bin/sudo', '#!/bin/sh\necho "$@" >> "$TRACE"\nexit 0\n', True)
            write(root, 'bin/dpkg-query', '#!/bin/sh\nexit 0\n', True)
            write(root, '_tests/adapters/build-lock-test.sh', 'exit 0\n')
            write(root, 'adapters/mesen2/build.sh', 'exit 0\n')
            trace = root / 'apt.log'
            result = subprocess.run(
                ['bash', str(RECIPE), 'mesen2', str(root)],
                env=dict(os.environ, PATH=str(root / 'bin') + ':' + os.environ['PATH'],
                         TRACE=str(trace)), text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            install = trace.read_text().splitlines()[-1].split()
            self.assertIn('zip', install)
            self.assertIn('libfontconfig1-dev', install)

    def test_pcsx2_cached_marker_and_modified_dependencies_are_never_consumed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name, body in [('uname', 'echo Linux'), ('sudo', 'exit 0'),
                               ('dpkg-query', 'exit 0'), ('git', 'exit 0')]:
                write(root, 'bin/' + name, '#!/bin/sh\n' + body + '\n', True)
            write(root, 'adapters/pcsx2/upstream.lock', 'PCSX2_REPO=fixture\nPCSX2_COMMIT=fixture\n')
            write(root, '_tests/adapters/build-lock-test.sh', 'exit 0\n')
            write(root, 'native-dependencies/pcsx2/.emucap-complete', '')
            write(root, 'native-dependencies/pcsx2/poison.cmake', 'untrusted')
            write(root, 'native-dependency-sources/pcsx2/poison.tar.gz', 'untrusted')
            write(root, 'adapters/pcsx2/work/pcsx2/.github/workflows/scripts/linux/build-dependencies-qt.sh',
                  'set -eu\n'
                  'test ! -e "$1/poison.cmake"\n'
                  'test ! -e poison.tar.gz\n'
                  'mkdir -p "$1/lib/pkgconfig"\n'
                  'echo rebuilt > "$1/fresh"\n')
            write(root, 'adapters/pcsx2/build.sh',
                  'set -eu\n'
                  'test ! -e "$CMAKE_PREFIX_PATH/poison.cmake"\n'
                  'test "$(cat "$CMAKE_PREFIX_PATH/fresh")" = rebuilt\n'
                  'test "$PKG_CONFIG_PATH" = "$CMAKE_PREFIX_PATH/lib/pkgconfig"\n'
                  'echo consumed-fresh > recipe-result\n')
            result = subprocess.run(['bash', str(RECIPE), 'pcsx2', str(root)],
                                    env=dict(os.environ, PATH=str(root / 'bin') + ':' + os.environ['PATH']),
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((root / 'recipe-result').read_text(), 'consumed-fresh\n')


if __name__ == '__main__': unittest.main()
