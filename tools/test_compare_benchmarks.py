"""Comparison tests use invented traces and metadata; no audio or network."""
import json
from pathlib import Path
import tempfile
import unittest

from compare_benchmarks import load_run, paths, render, series


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.before = Path(self.temporary.name) / 'before'
        self.after = Path(self.temporary.name) / 'after'
        for directory in (self.before, self.after):
            self.fixture(directory)

    def fixture(self, directory):
        case = directory / 'example' / 'middle'
        case.mkdir(parents=True)
        self.write(directory / 'run.json', {'complete': True, 'cases': True, 'sample_rate': 44100,
                                          'model': 1, 'manifest_sha256': 'same-reference'})
        self.write(directory / 'report.json', [{
            'id': 'example', 'case_id': 'middle', 'backend': 'beatnet', 'mode': 'assisted',
            'reference': 'approximate_tap', 'model': 1, 'source_start_seconds': 180,
            'duration_seconds': 40, 'bpm_tolerance': 2, 'sustain_seconds': 5,
            'acquire_within_seconds': 30, 'status': 'pass', 'first_lock_seconds': 1,
            'correct_lock_confirmed_seconds': 6, 'correct_after_deadline_percent': 100,
            'double_tempo_percent': 0, 'half_tempo_percent': 0, 'clock_bpm_mae': 0,
            'phase_jumps_over_40ms_per_min': 0,
        }])
        self.write(case / 'example.meta.json', {'source_sha256': 'audio-hash', 'model': 1,
                                               'sample_rate': 44100, 'source_start_seconds': 180,
                                               'duration': 40})
        rows = [{'time': time, 'estimate': {'bpm': 120, 'grids': [None, {'period': .6}, None]}}
                for time in [1, 2, 40]]
        (case / 'example.beatnet.assisted.jsonl').write_text('\n'.join(map(json.dumps, rows)))

    def write(self, path, value):
        path.write_text(json.dumps(value))

    def change_row(self, **values):
        path = self.after / 'report.json'
        rows = json.loads(path.read_text())
        rows[0].update(values)
        self.write(path, rows)

    def test_display_and_clock_are_distinct_and_no_reference_is_invented(self):
        row = {'time': 1, 'estimate': {'bpm': 120, 'grids': [None, {'period': .6}, None]}}
        self.assertEqual(series([row]), [(1, 120)])
        self.assertEqual(series([row], True), [(1, 100)])
        document = render(self.before, self.after)
        self.assertIn('Displayed tempo · BPM', document)
        self.assertIn('Actual output clock · BPM', document)
        self.assertIn('No reference line is inferred', document)
        self.assertIn('1 matching assisted cases', document)
        self.assertIn('same status: 1', document)

    def test_regressions_remain_visible_when_status_stays_pass(self):
        self.change_row(correct_lock_confirmed_seconds=10, double_tempo_percent=3)
        document = render(self.before, self.after)
        self.assertIn('data-regression="1" data-fail="0"', document)
        self.assertIn('Correct lock confirmed · s, Double tempo · %', document)
        self.assertIn('+4.00', document)

    def test_lost_lock_is_not_treated_as_zero_latency(self):
        self.change_row(correct_lock_confirmed_seconds=None, status='fail')
        document = render(self.before, self.after)
        self.assertIn('pass → fail: 1', document)
        self.assertIn('data-regression="1"', document)
        self.assertIn('<td>6.00</td><td>—</td><td>—</td>', document)

    def test_scoring_or_reference_changes_require_explicit_override(self):
        self.change_row(bpm_tolerance=5)
        with self.assertRaisesRegex(ValueError, 'bpm_tolerance differs'):
            render(self.before, self.after)
        self.assertIn('EXPLORATORY', render(self.before, self.after, True))

    def test_manifest_hash_and_source_audio_must_match(self):
        for relative, field in [('run.json', 'manifest_sha256'),
                                ('example/middle/example.meta.json', 'source_sha256')]:
            with self.subTest(field=field):
                path = self.after / relative
                original = json.loads(path.read_text())
                self.write(path, original | {field: 'changed'})
                with self.assertRaisesRegex(ValueError, field):
                    render(self.before, self.after)
                self.write(path, original)

    def test_case_mismatch_is_not_silently_dropped(self):
        path = self.after / 'report.json'
        rows = json.loads(path.read_text())
        self.write(path, rows + [rows[0] | {'mode': 'stable'}])
        with self.assertRaisesRegex(ValueError, 'identities are missing'):
            render(self.before, self.after)

    def test_incomplete_runs_and_duplicate_or_unsafe_ids_are_rejected(self):
        path = self.after / 'run.json'
        run = json.loads(path.read_text())
        self.write(path, run | {'complete': False})
        with self.assertRaisesRegex(ValueError, 'incomplete'):
            load_run(self.after)
        self.write(path, run)
        path = self.after / 'report.json'
        rows = json.loads(path.read_text())
        self.write(path, rows * 2)
        with self.assertRaisesRegex(ValueError, 'Duplicate'):
            load_run(self.after)
        self.write(path, [rows[0] | {'id': '../private'}])
        with self.assertRaisesRegex(ValueError, 'safe'):
            load_run(self.after)

    def test_missing_estimate_breaks_the_plotted_line(self):
        result = paths([(0, 120), (1, 120), (2, None), (3, 120)], 40, 100, 140)
        self.assertEqual(len(result), 2)
        self.assertTrue(all(path.startswith('M') for path in result))


if __name__ == '__main__':
    unittest.main()
