import unittest

import input_pacing


class InputTimingTests(unittest.TestCase):
    def test_advertised_poll_boundary_is_preferred(self):
        status = {'input_poll_capability': {'step_method': 'step_input_polls'}}
        self.assertEqual(input_pacing.input_timing({}, status), ('input_polls', 1, 7))
        self.assertEqual(
            input_pacing.input_timing({'poll_press': 2, 'poll_after': 5}, status),
            ('input_polls', 2, 5),
        )

    def test_frames_remain_the_fallback(self):
        self.assertEqual(input_pacing.input_timing({}, {}), ('frames', 2, 30))
        self.assertEqual(
            input_pacing.input_timing({'press_frames': 4, 'after_frames': 9}, {}),
            ('frames', 4, 9),
        )


if __name__ == '__main__':
    unittest.main()
