#!/usr/bin/env python3
from pathlib import Path
import runpy
import sys
import unittest


class MediaChangeImportTests(unittest.TestCase):
    def test_imports_sibling_with_isolated_module_search_path(self):
        script = Path(__file__).with_name('media_change.py')
        original = sys.path[:]
        try:
            sys.path[:] = [entry for entry in sys.path if Path(entry or '.').resolve() != script.parent]
            namespace = runpy.run_path(str(script), run_name='media_change_import_test')
        finally:
            sys.path[:] = original
        self.assertTrue(callable(namespace['fixture']))


if __name__ == '__main__':
    unittest.main()
