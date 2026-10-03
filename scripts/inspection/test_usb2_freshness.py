"""Pure statistics fixtures; never open a camera."""
import unittest
from validate_asi6200 import validate_full_frame


class FullFrameFreshnessTests(unittest.TestCase):
    def test_stale_rows_are_rejected_even_with_correct_band_medians(self):
        options = dict(width=9576, microseconds=32, gain=100, offset=50)
        result = dict(statistics=dict(rowMedianRange=[500, 2000],
                                     rowBandMedians=[500] * 16, median=500))
        with self.assertRaisesRegex(RuntimeError, 'every row'):
            validate_full_frame(options, [result])
        result['statistics']['rowMedianRange'] = [499, 502]
        validate_full_frame(options, [result])

    def test_other_gain_still_rejects_nonuniform_rows(self):
        options = dict(width=9576, microseconds=100000, gain=280, offset=50)
        result = dict(statistics=dict(rowMedianRange=[500, 700], median=500))
        with self.assertRaisesRegex(RuntimeError, 'nonuniform'):
            validate_full_frame(options, [result])

    def test_saved_usb3_high_gain_also_triggers_conservative_gate(self):
        # This is a documented limitation of the diagnostic, not a USB 2
        # regression claim. Do not turn this into a calibrated noise threshold.
        options = dict(width=9576, microseconds=100000, gain=520, offset=200)
        result = dict(statistics=dict(rowMedianRange=[1590, 1856], median=1728))
        with self.assertRaisesRegex(RuntimeError, 'nonuniform'):
            validate_full_frame(options, [result])


if __name__ == '__main__':
    unittest.main()
