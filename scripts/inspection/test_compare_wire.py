import unittest
import numpy as np
from compare_wire import compare, complete_runs


class WireTests(unittest.TestCase):
    def test_never_stitches_across_a_missing_request(self):
        frames, rejected = complete_runs([1, 2, 3, 4, 5], {1: b'ab', 3: b'cd', 4: b'ef', 5: b'gh'}, 8)
        self.assertEqual(frames, [])
        self.assertEqual(rejected, [2, 6])

    def test_complete_replay_is_separate_from_incomplete_pass(self):
        frames, rejected = complete_runs([1, 2, 3, 4, 5, 6],
                                         {1: b'XX', 3: b'ab', 4: b'cd', 5: b'ab', 6: b'cd'}, 4)
        self.assertEqual(frames, [b'abcd', b'abcd'])
        self.assertEqual(rejected, [2])

    def test_oversized_or_short_payloads_do_not_look_complete(self):
        self.assertEqual(complete_runs([1, 2], {1: b'abcde', 2: b'abc'}, 4), ([], [5, 3]))
        self.assertFalse(compare(b'ab', b'cd', 8, 2)['comparable'])

    def test_exact_endian_and_scaling_detection(self):
        source = np.arange(1, 17, dtype=np.uint16).reshape(2, 8) * 17
        frame = (source << 4).astype('<u2').tobytes()
        result = compare(source.astype('>u2').tobytes(), frame, 8, 2)
        self.assertEqual(result['candidates'][0],
                         {'endian': '>', 'shift': 4, 'matchedPixels': 16, 'totalPixels': 16, 'exact': True})

    def test_sparse_processing_differences_do_not_report_exact(self):
        source = np.arange(16, dtype='<u2') * 257
        frame = source.copy()
        frame[7] += 1
        result = compare(source.tobytes(), frame.tobytes(), 8, 2)
        self.assertEqual(result['differences']['count'], 1)
        self.assertEqual(result['candidates'][0]['matchedPixels'], 15)
        self.assertFalse(result['candidates'][0]['exact'])


if __name__ == '__main__':
    unittest.main()
