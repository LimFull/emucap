"""Runtime shell discovery must agree with the Rust user-data boundary."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class RuntimeHomeTests(unittest.TestCase):
    def shell(self, system, variables, command="emucap_runtime_base"):
        env = dict(os.environ)
        for key in ("HOME", "XDG_DATA_HOME", "LOCALAPPDATA", "EMUCAP_EMU_HOME",
                    "EMUCAP_SESSION_TOKEN_FILE"):
            env.pop(key, None)
        env.update(variables)
        return subprocess.run(
            ["bash", "-c", f'uname() {{ echo "{system}"; }}\n'
             '. adapters/_common/runtime-env.sh\n' + command],
            cwd=ROOT, env=env, text=True, capture_output=True, timeout=5,
        )

    def test_explicit_root_preserves_path_on_all_platforms(self):
        with tempfile.TemporaryDirectory() as directory:
            for system in ("Darwin", "Linux", "MINGW64_NT-10.0"):
                result = self.shell(system, {"EMUCAP_EMU_HOME": directory},
                                    "emucap_session_token_file 47800")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(),
                                 f"{directory}/sessions/compatibility/session-token-47800")

    def test_invalid_root_does_not_construct_a_token_path(self):
        for system in ("Darwin", "Linux", "MINGW64_NT-10.0"):
            for value in ("", "relative", "../shared"):
                result = self.shell(system, {"EMUCAP_EMU_HOME": value},
                                    "emucap_session_token_file 47800")
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def test_windows_missing_localappdata_does_not_use_temp(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.shell("MINGW64_NT-10.0", {"TEMP": directory},
                                "emucap_session_token_file 47800")
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_native_windows_absolute_path(self):
        result = self.shell("MINGW64_NT-10.0", {"LOCALAPPDATA": "C:/Users/test/AppData/Local"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "C:/Users/test/AppData/Local/emucap")
        self.assertNotEqual(self.shell("Linux", {"EMUCAP_EMU_HOME": "C:/relative"}).returncode, 0)

    def test_linux_xdg_precedence_and_validation(self):
        result = self.shell("Linux", {"XDG_DATA_HOME": "/data", "HOME": "invalid"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/data/emucap")
        for value in ("", "relative"):
            self.assertNotEqual(self.shell("Linux", {"XDG_DATA_HOME": value}).returncode, 0)

    def test_relative_home_is_rejected(self):
        for system in ("Darwin", "Linux"):
            result = self.shell(system, {"HOME": "relative"})
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
