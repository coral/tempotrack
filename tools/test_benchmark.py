"""Tests for scoring, using only invented timelines (no private audio)."""
import unittest
from benchmark import metrics


def row(time, bpm=120, anchor=0):
    return {'time': time, 'estimate': {'bpm': bpm, 'grids': [None, {'anchor': anchor, 'period': 60 / bpm} if bpm else None, None]}}


class MetricsTests(unittest.TestCase):
    def test_coverage_counts_unacquired_time(self):
        result = metrics([row(1)], [{'start': 0, 'end': 2, 'bpm': 120}], 2)
        self.assertEqual(result['coverage_percent'], 50)
        self.assertEqual(result['within_1_bpm_percent'], 50)
        self.assertEqual(result['bpm_mae'], 0)
        self.assertIsNone(result['absolute_phase_error_ms_p95'])

    def test_beat_renumbering_is_not_a_phase_jump(self):
        result = metrics([row(0), row(.5, anchor=.5)], [{'bpm': 120}], 1)
        self.assertEqual(result['phase_jumps_over_40ms_per_min'], 0)

    def test_phase_error_requires_explicit_annotation(self):
        result = metrics([row(0, anchor=.125)], [{'bpm': 120, 'beat_offset': 0}], 2)
        self.assertAlmostEqual(result['absolute_phase_error_ms_p95'], 125)

    def test_phase_correction_and_true_clock_tempo_are_measured(self):
        result = metrics([row(0), row(1, anchor=.1)], [{'bpm': 120}], 2)
        self.assertEqual(result['phase_jumps_over_40ms_per_min'], 30)
        changed = row(0)
        changed['estimate']['grids'][1]['period'] = .6
        result = metrics([changed], [{'bpm': 120}], 2)
        self.assertEqual(result['bpm_mae'], 0)
        self.assertEqual(result['clock_bpm_mae'], 20)

    def test_segment_truth_is_used_only_inside_its_window(self):
        result = metrics([row(0, 100), row(1, 120)], [{'start': 1, 'end': 2, 'bpm': 120}], 3)
        self.assertEqual(result['evaluated_seconds'], 1)
        self.assertEqual(result['bpm_mae'], 0)


if __name__ == '__main__':
    unittest.main()
