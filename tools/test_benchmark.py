"""Tests for scoring, using only invented timelines (no private audio)."""
import unittest
import contextlib
import io
import json
from pathlib import Path
import tempfile
from unittest.mock import patch
from benchmark import main, metrics, session_segments


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


if __name__ == '__main__':
    unittest.main()
