"""Tests for scoring, using only invented timelines (no private audio)."""
import unittest
import contextlib
import io
import json
from pathlib import Path
import tempfile
from unittest.mock import patch
from argparse import Namespace
from html.parser import HTMLParser
from benchmark import main, metrics, session_segments, case_specs, resolve_case, validate_cache, write_reports


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


class AcquisitionTests(unittest.TestCase):
    def test_double_tempo_is_acquired_but_not_correct(self):
        result = metrics([row(2, 194), row(12, 97)], [{'bpm': 97}], 40)
        self.assertEqual(result['first_lock_seconds'], 2)
        self.assertEqual(result['first_correct_lock_seconds'], 12)
        self.assertEqual(result['correct_lock_confirmed_seconds'], 17)
        self.assertEqual(result['double_tempo_percent'], 25)
        self.assertEqual(result['correct_after_deadline_percent'], 100)
        self.assertEqual(result['status'], 'pass')

    def test_persistent_double_and_half_tempo_fail(self):
        for bpm, metric in [(194, 'double_tempo_percent'), (48.5, 'half_tempo_percent')]:
            with self.subTest(bpm=bpm):
                result = metrics([row(0, bpm)], [{'bpm': 97}], 40)
                self.assertEqual(result[metric], 100)
                self.assertIsNone(result['first_correct_lock_seconds'])
                self.assertEqual(result['status'], 'fail')

    def test_temporary_correct_estimate_does_not_count_as_lock(self):
        result = metrics([row(0, 97), row(4.98, 194), row(20, 97)], [{'bpm': 97}], 40)
        self.assertEqual(result['first_correct_lock_seconds'], 20)
        self.assertEqual(result['correct_lock_confirmed_seconds'], 25)

    def test_late_correct_lock_fails_even_with_perfect_final_coverage(self):
        result = metrics([row(27, 97)], [{'bpm': 97}], 40)
        self.assertEqual(result['correct_after_deadline_percent'], 100)
        self.assertEqual(result['status'], 'fail')

    def test_regression_after_acquisition_is_a_failure(self):
        result = metrics([row(0, 97), row(35, 194)], [{'bpm': 97}], 40)
        self.assertEqual(result['correct_lock_confirmed_seconds'], 5)
        self.assertEqual(result['correct_after_deadline_percent'], 50)
        self.assertEqual(result['status'], 'fail')

    def test_display_correct_but_clock_wrong_does_not_pass(self):
        estimate = row(0, 97)
        estimate['estimate']['grids'][1]['period'] = 60 / 194
        result = metrics([estimate], [{'bpm': 97}], 40)
        self.assertEqual(result['correct_tempo_percent'], 0)
        self.assertEqual(result['double_tempo_percent'], 100)
        self.assertEqual(result['status'], 'fail')

    def test_unannotated_gaps_break_sustained_lock(self):
        result = metrics([row(0, 97)], [{'start': 0, 'end': 3, 'bpm': 97},
                                       {'start': 10, 'bpm': 97}], 40)
        self.assertEqual(result['first_correct_lock_seconds'], 10)
        self.assertEqual(result['correct_lock_confirmed_seconds'], 15)

    def test_no_reference_and_short_sessions_are_unscored(self):
        for segments, duration in [([], 40), ([{'start': 100, 'bpm': 97}], 40), ([{'bpm': 97}], 32)]:
            with self.subTest(segments=segments, duration=duration):
                result = metrics([row(0, 97)], segments, duration)
                self.assertEqual(result['status'], 'unscored')
                self.assertTrue(result['failure_reason'])

    def test_tolerance_and_deadline_are_configurable(self):
        result = metrics([row(2, 98.5)], [{'bpm': 97}], 20, tolerance=2, deadline=8)
        self.assertEqual(result['status'], 'pass')
        result = metrics([row(2, 98.5)], [{'bpm': 97}], 20, tolerance=1, deadline=8)
        self.assertEqual(result['status'], 'fail')


class ReportTests(unittest.TestCase):
    def test_passing_cases_with_early_octave_errors_remain_discoverable(self):
        class Rows(HTMLParser):
            def __init__(self):
                super().__init__()
                self.rows = []

            def handle_starttag(self, tag, attrs):
                attrs = dict(attrs)
                if tag == 'tr' and 'data-status' in attrs:
                    self.rows.append(attrs)

        report = []
        for identity, timeline in [('doubled', [row(0, 194), row(12, 97)]),
                                   ('halved', [row(0, 48.5), row(4, 97)]),
                                   ('correct', [row(0, 97)])]:
            report.append({'id': identity, 'case_id': 'middle', 'source_start_seconds': 180,
                           'duration_seconds': 40, 'backend': 'beatnet', 'mode': 'assisted',
                           **metrics(timeline, [{'bpm': 97}], 40)})
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            write_reports(directory, report, [])
            parsed = Rows()
            parsed.feed((directory / 'report.html').read_text())
        self.assertEqual([row['data-status'] for row in parsed.rows], ['pass'] * 3)
        self.assertEqual([row['data-octave'] for row in parsed.rows], ['1', '1', '0'])
        self.assertEqual([row['data-slow'] for row in parsed.rows], ['1', '0', '0'])


class CaseSelectionTests(unittest.TestCase):
    def args(self, **changes):
        return Namespace(**({'cases': True, 'start': 0., 'limit': None, 'case_duration': 90.,
                             'acquire_within': 30., 'midpoints': None} | changes))

    def test_defaults_include_start_and_midpoint_without_reusing_state(self):
        specs = case_specs({}, self.args())
        self.assertEqual([spec['id'] for spec in specs], ['start', 'fraction-0_5'])
        self.assertEqual([resolve_case(spec, 300) for spec in specs], [0, 150])
        self.assertEqual([spec['duration'] for spec in specs], [90, 90])

    def test_explicit_cases_and_deadline_override(self):
        track = {'cases': [{'id': 'cue', 'start': 180, 'duration': 20, 'acquire_within': 8},
                           {'id': 'middle', 'fraction': .5}]}
        specs = case_specs(track, self.args(limit=60))
        self.assertEqual([spec['duration'] for spec in specs], [20, 60])
        self.assertEqual([spec['acquire_within'] for spec in specs], [8, 30])
        self.assertEqual(resolve_case(specs[1], 400), 200)
        self.assertNotIn('duration', track['cases'][1])

    def test_invalid_cases_are_rejected_before_decoding(self):
        for case in [{'id': '../escape', 'start': 0}, {'id': 'bad', 'start': -1},
                     {'id': 'bad', 'fraction': 1}, {'id': 'bad', 'fraction': float('nan')},
                     {'id': 'bad', 'start': 0, 'fraction': .5}, {'id': 'bad'},
                     {'id': 'bad', 'start': 0, 'duration': 0}]:
            with self.subTest(case=case), self.assertRaises(ValueError):
                case_specs({'cases': [case]}, self.args())
        with self.assertRaises(ValueError):
            resolve_case({'id': 'beyond', 'start': 100}, 100)

    def test_cache_rejects_changed_duration_or_start(self):
        metadata = {'sample_rate': 44100, 'model': 1, 'source_start_seconds': 180,
                    'duration': 90, 'requested_duration_seconds': 90}
        validate_cache(metadata, 44100, 1, 180, 90, True)
        for start, duration in [(181, 90), (180, 60)]:
            with self.assertRaises(ValueError):
                validate_cache(metadata, 44100, 1, start, duration, True)


class ColdStartTests(unittest.TestCase):
    def test_reference_windows_shift_without_mutating_the_manifest(self):
        segments = [
            {'start': 0, 'end': 10, 'bpm': 100},
            {'start': 10, 'end': 20, 'bpm': 120, 'beat_offset': 10},
            {'start': 30, 'bpm': 140},
        ]
        shifted = session_segments(segments, 12.25)
        self.assertEqual(shifted, [
            {'start': 0, 'end': 7.75, 'bpm': 120, 'beat_offset': -2.25},
            {'start': 17.75, 'bpm': 140},
        ])
        self.assertEqual(segments[1]['beat_offset'], 10)
        result = metrics([row(0, anchor=.25)], shifted, 5)
        self.assertAlmostEqual(result['absolute_phase_error_ms_p95'], 0)
        self.assertEqual(session_segments(segments, 10)[0]['bpm'], 120)

    def test_decode_offset_and_cache_metadata_follow_the_same_session(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            manifest = directory / 'tracks.json'
            source = directory / 'synthetic.wav'
            source.write_bytes(b'synthetic input')
            manifest.write_text(json.dumps({'sample_rate': 8000, 'tracks': [{
                'id': 'synthetic', 'file': source.name,
                'segments': [{'bpm': 120, 'beat_offset': 0}],
            }]}))
            output = directory / 'out'
            commands = []

            def run(command, **kwargs):
                commands.append(command)
                if command[0] == 'ffmpeg':
                    Path(command[-1]).write_bytes(bytes(4 * 8000))
                elif command[0].endswith('track-replay'):
                    kwargs['stdout'].write(json.dumps(row(0, anchor=-12.25)) + '\n')

            argv = ['benchmark.py', str(manifest), '--out', str(output),
                    '--backend', 'beatnet', '--start', '12.25', '--limit', '1']
            with patch('sys.argv', argv), patch('benchmark.subprocess.run', side_effect=run), \
                    patch('benchmark.subprocess.check_output', return_value=b'{"streams":[{"channels":1}]}'), \
                    contextlib.redirect_stdout(io.StringIO()):
                main()
            decode = next(command for command in commands if command[0] == 'ffmpeg')
            self.assertEqual(decode[decode.index('-ss') + 1], '12.25')
            self.assertLess(decode.index('-ss'), decode.index('-i'))
            self.assertEqual(decode[decode.index('-t') + 1], '1.0')
            metadata = json.loads((output / 'synthetic.meta.json').read_text())
            self.assertEqual(metadata['source_start_seconds'], 12.25)
            self.assertEqual(metadata['duration'], 1)
            self.assertEqual(metadata, json.loads((output / 'synthetic.beatnet.raw.meta.json').read_text()))
            report = json.loads((output / 'report.json').read_text())
            self.assertAlmostEqual(report[0]['absolute_phase_error_ms_p95'], 0)
            self.assertEqual(report[0]['source_start_seconds'], 12.25)

            # A cached offset must be explicit; silently treating it as a full-file
            # run would compare the observations with the wrong reference phase.
            cached = ['benchmark.py', str(manifest), '--out', str(output), '--backend', 'beatnet', '--cached']
            with patch('sys.argv', cached), patch('benchmark.subprocess.run'), \
                    contextlib.redirect_stderr(io.StringIO()) as error:
                with self.assertRaises(SystemExit):
                    main()
            self.assertIn('cached source start differs', error.getvalue())
            with patch('sys.argv', cached + ['--start', '12.25']), \
                    patch('benchmark.subprocess.run', side_effect=run), \
                    contextlib.redirect_stdout(io.StringIO()):
                main()


class CaseRunnerTests(unittest.TestCase):
    def test_cases_are_isolated_filtered_and_checked_without_feeding_truth(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            manifest = directory / 'tracks.json'
            source = directory / 'synthetic.wav'
            source.write_bytes(b'synthetic input')
            contents = {'sample_rate': 8000, 'tracks': [{
                'id': 'synthetic', 'file': source.name,
                'segments': [{'bpm': 120}],
                'cases': [{'id': 'opening', 'start': 0, 'duration': 40},
                          {'id': 'middle', 'fraction': .5, 'duration': 40}],
            }]}
            manifest.write_text(json.dumps(contents))
            output = directory / 'out'
            commands = []

            def run(command, **kwargs):
                commands.append(command)
                if command[0] == 'ffmpeg':
                    Path(command[-1]).write_bytes(bytes(4 * 8000 * 40))
                elif command[0].endswith('track-replay'):
                    # Only the assisted path finds the right octave in this fixture.
                    bpm = 120 if '--guide' in command else 240
                    kwargs['stdout'].write(json.dumps(row(0, bpm)) + '\n')

            argv = ['benchmark.py', str(manifest), '--out', str(output), '--cases', '--check']
            probe = b'{"streams":[{"channels":1,"duration":"100"}]}'
            with patch('sys.argv', argv), patch('benchmark.subprocess.run', side_effect=run), \
                    patch('benchmark.subprocess.check_output', return_value=probe), \
                    contextlib.redirect_stdout(io.StringIO()):
                main()
            self.assertEqual(sum(command[0] == 'cargo' for command in commands), 1)
            decode = [command for command in commands if command[0] == 'ffmpeg']
            self.assertEqual(len(decode), 2)
            self.assertNotIn('-ss', decode[0])
            self.assertEqual(decode[1][decode[1].index('-ss') + 1], '50.0')
            for command in commands:
                if command[0].endswith('track-replay'):
                    self.assertNotIn('--bpm', command)
                    self.assertNotIn('120', command)
            report = json.loads((output / 'report.json').read_text())
            assisted = [result for result in report if result['mode'] == 'assisted']
            self.assertEqual([r['case_id'] for r in assisted], ['opening', 'middle'])
            self.assertEqual([r['source_start_seconds'] for r in assisted], [0, 50])
            self.assertEqual([r['status'] for r in assisted], ['pass', 'pass'])
            self.assertEqual(sum(r['status'] == 'fail' for r in report), 8)
            self.assertTrue((output / 'report.csv').exists())
            self.assertIn('id="status"', (output / 'report.html').read_text())
            meta_paths = [output / 'synthetic' / case / 'synthetic.meta.json' for case in ['opening', 'middle']]
            self.assertEqual([json.loads(path.read_text())['source_start_seconds'] for path in meta_paths], [0, 50])

            commands.clear()
            with patch('sys.argv', argv + ['--cached', '--case', 'middle']), \
                    patch('benchmark.subprocess.run', side_effect=run), \
                    patch('benchmark.subprocess.check_output') as probe_call, \
                    contextlib.redirect_stdout(io.StringIO()):
                main()
            probe_call.assert_not_called()
            self.assertFalse(any(command[0] == 'ffmpeg' or '--pcm' in command for command in commands))
            self.assertEqual({r['case_id'] for r in json.loads((output / 'report.json').read_text())}, {'middle'})

            # Re-scoring is allowed, but unknown references must never pass --check.
            contents['tracks'][0]['segments'] = []
            manifest.write_text(json.dumps(contents))
            with patch('sys.argv', argv + ['--cached', '--case', 'middle']), \
                    patch('benchmark.subprocess.run', side_effect=run), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    main()
            self.assertEqual(error.exception.code, 1)
            report = json.loads((output / 'report.json').read_text())
            self.assertTrue(all(r['status'] == 'unscored' for r in report))
            self.assertTrue(json.loads((output / 'run.json').read_text())['complete'])

    def test_unknown_case_is_rejected_before_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            manifest = Path(temporary) / 'tracks.json'
            manifest.write_text(json.dumps({'tracks': [{'id': 'synthetic', 'file': 'unused', 'segments': []}]}))
            with patch('sys.argv', ['benchmark.py', str(manifest), '--cases', '--case', 'missing']), \
                    patch('benchmark.subprocess.run') as run, contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    main()
            self.assertEqual(error.exception.code, 2)
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
