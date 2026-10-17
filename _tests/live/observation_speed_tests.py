#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest


PATH = Path(__file__).with_name('observation_speed.py')
SPEC = importlib.util.spec_from_file_location('observation_speed_under_test', PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Witness:
    def __init__(self, replies):
        self.replies = iter(replies)
        self.counts = []

    def call(self, tool, arguments):
        self.assertions(tool, arguments)
        return next(self.replies)

    def assertions(self, tool, arguments):
        assert tool == 'step'
        assert arguments['unit'] == 'frames'
        self.counts.append(arguments['count'])


class WarmupTests(unittest.TestCase):
    def test_continues_after_progressing_host_deadline(self):
        witness = Witness([
            {'status': 'interrupted', 'reason': 'host_deadline', 'completed': 2931},
            {'status': 'completed', 'completed': 669},
        ])
        MODULE.warmup(witness, {'execution_limits': {'frame': {'max_count': 5000}}}, 3600)
        self.assertEqual(witness.counts, [3600, 669])

    def test_rejects_interruption_without_progress(self):
        witness = Witness([
            {'status': 'interrupted', 'reason': 'host_deadline', 'completed': 0},
        ])
        with self.assertRaises(AssertionError):
            MODULE.warmup(witness, {'execution_limits': {'frame': {'max_count': 5000}}}, 3600)

    def test_rejects_unrelated_interruption(self):
        witness = Witness([
            {'status': 'interrupted', 'reason': 'breakpoint', 'completed': 10},
        ])
        with self.assertRaises(AssertionError):
            MODULE.warmup(witness, {'execution_limits': {'frame': {'max_count': 5000}}}, 3600)


if __name__ == '__main__':
    unittest.main()
