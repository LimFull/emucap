"""Exercise production shell launch fragments without launching emulators."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class LauncherCredentials(unittest.TestCase):
    def run_shell(self, body, directory):
        result = subprocess.run(
            ["bash", "-c", body], cwd=ROOT, text=True, capture_output=True,
            env={**os.environ, "RUN_DIR": directory},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("secret-selected", result.stdout)
        return result.stdout

    def test_media_scripts_rejected_before_launch_preparation(self):
        with tempfile.TemporaryDirectory() as directory:
            for name in ("payload.lua", "payload.LuA", ".lua", ".LuA"):
                path = Path(directory) / name
                path.write_text("os.execute('unused')")
                result = subprocess.run(
                    ["bash", str(ROOT / "adapters/mesen2/launch.sh"), str(path), "12345", "", "snes"],
                    capture_output=True, text=True, timeout=5,
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn("not a Lua script", result.stderr)

    def test_launchservices_file(self):
        source = (ROOT / "adapters/mesen2/launch.sh").read_text()
        start = source.index('  OPEN_ENV=(--env')
        end = source.index('\nelse\n', start)
        with tempfile.TemporaryDirectory() as directory:
            output = self.run_shell(
                'SESSION_TOKEN=secret-selected\n'
                'open() { printf "%s\\n" "$@"; }\n' + source[start:end]
                + '\n[ "$(cat "$SESSION_TOKEN_PATH")" = "$SESSION_TOKEN" ]',
                directory,
            )
            self.assertIn("EMUCAP_SESSION_TOKEN_FILE=", output)
            files = list(Path(directory).iterdir())
            self.assertEqual(len(files), 1)
            self.assertEqual(files[0].stat().st_mode & 0o777, 0o600)

    def test_direct_invocations(self):
        for adapter in ("mesen2", "flycast"):
            source = (ROOT / f"adapters/{adapter}/launch.sh").read_text()
            start = source.index('env EMUCAP_PORT="$PORT"')
            end = source.index(' &\n', start) + 2
            fragment = source[start:end]
            with tempfile.TemporaryDirectory() as directory:
                self.run_shell(
                    'SESSION_TOKEN=secret-selected\nexport EMUCAP_SESSION_TOKEN="$SESSION_TOKEN"\n'
                    'LOG=/dev/stdout\nLAUNCH_LOG=/dev/stdout\n'
                    'env() { [ "$EMUCAP_SESSION_TOKEN" = secret-selected ] || return 1; '
                    'printf "%s\\n" "$@"; }\n' + fragment + '\nwait',
                    directory,
                )


if __name__ == "__main__":
    unittest.main()
