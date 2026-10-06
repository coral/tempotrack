#!/usr/bin/env python3
"""Offline, causal benchmark. Python stdlib + ffmpeg; ground truth never enters tracking."""
import argparse
import bisect
import csv
import html
import hashlib
import json
import math
from pathlib import Path
import statistics
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def quantile(values, fraction):
    return sorted(values)[min(len(values) - 1, int(fraction * len(values)))] if values else None


def session_segments(segments, start):
    """Translate source annotations to a fresh tracker session without changing phase."""
    shifted = []
    for segment in segments:
        if segment.get('end', math.inf) <= start:
            continue
        segment = dict(segment)
        segment['start'] = max(0, segment.get('start', 0) - start)
        if 'end' in segment:
            segment['end'] -= start
        if 'beat_offset' in segment:
            segment['beat_offset'] -= start
        shifted.append(segment)
    return shifted


def metrics(rows, segments, duration, tolerance=2., sustain=5., deadline=30.):
    times = [r['time'] for r in rows]
    errors, bpms, phases, changes, clock_errors = [], [], [], [], []
    evaluated = 0
    holding = 0
    jumps = 0
    correct = doubled = halved = after_deadline = correct_after_deadline = 0
    correct_run_start = confirmed_at = first_correct = last_evaluated = None
    for segment in segments:
        start, end = segment.get('start', 0), min(segment.get('end', duration), duration)
        previous = None
        for tick in range(math.ceil(start * 50), math.floor(end * 50)):
            t = tick / 50
            evaluated += 1
            eligible = t >= deadline
            after_deadline += eligible
            if last_evaluated is not None and t - last_evaluated > .021:
                correct_run_start = None
            last_evaluated = t
            index = bisect.bisect_right(times, t) - 1
            if index < 0:
                correct_run_start = None
                continue
            estimate = rows[index]['estimate']
            grid = estimate['grids'][1]
            if grid is None or estimate['bpm'] is None:
                correct_run_start = None
                continue
            holding += bool(estimate.get('holding', False))
            bpm = estimate['bpm']
            clock_bpm = 60 / grid['period']
            is_correct = abs(bpm - segment['bpm']) <= tolerance and abs(clock_bpm - segment['bpm']) <= tolerance
            correct += is_correct
            correct_after_deadline += eligible and is_correct
            doubled += abs(clock_bpm / 2 - segment['bpm']) <= tolerance
            halved += abs(clock_bpm * 2 - segment['bpm']) <= tolerance
            if is_correct:
                if correct_run_start is None:
                    correct_run_start = t
                if confirmed_at is None and t + .02 - correct_run_start >= sustain - 1e-8:
                    first_correct = correct_run_start
                    confirmed_at = t + .02
            else:
                correct_run_start = None
            errors.append(abs(bpm - segment['bpm']))
            bpms.append(bpm)
            clock_errors.append(abs(60 / grid['period'] - segment['bpm']))
            if previous is not None:
                changes.append(abs(bpm - previous['bpm']))
                old = previous['grids'][1]
                correction = ((t - grid['anchor']) / grid['period'] - (t - old['anchor']) / old['period'] + .5) % 1 - .5
                jumps += abs(correction * grid['period']) > .04
            previous = estimate
            if 'beat_offset' in segment:
                phase = ((t - grid['anchor']) / grid['period'] - (t - segment['beat_offset']) * segment['bpm'] / 60 + .5) % 1 - .5
                phases.append(abs(phase * 60000 / segment['bpm']))
    correct_coverage = 100 * correct_after_deadline / after_deadline if after_deadline else None
    if not evaluated:
        status, reason = 'unscored', 'no reference overlaps the session'
    elif after_deadline / 50 < sustain:
        status, reason = 'unscored', 'less than one sustained-lock window is annotated after the deadline'
    elif confirmed_at is None or confirmed_at > deadline + 1e-8:
        status, reason = 'fail', 'correct tempo was not sustained by the acquisition deadline'
    elif correct_coverage < 90:
        status, reason = 'fail', 'correct tempo coverage after acquisition is below 90%'
    else:
        status, reason = 'pass', ''
    return {
        'status': status,
        'failure_reason': reason,
        'bpm_tolerance': tolerance,
        'sustain_seconds': sustain,
        'acquire_within_seconds': deadline,
        'first_correct_lock_seconds': first_correct,
        'correct_lock_confirmed_seconds': confirmed_at,
        'correct_tempo_percent': 100 * correct / evaluated if evaluated else None,
        'double_tempo_percent': 100 * doubled / evaluated if evaluated else None,
        'half_tempo_percent': 100 * halved / evaluated if evaluated else None,
        'evaluated_after_deadline_seconds': after_deadline / 50,
        'correct_after_deadline_percent': correct_coverage,
        'evaluated_seconds': evaluated / 50,
        'holdover_percent': 100 * holding / evaluated if evaluated else None,
        'coverage_percent': 100 * len(errors) / evaluated if evaluated else None,
        'median_bpm': statistics.median(bpms) if bpms else None,
        'bpm_p05': quantile(bpms, .05),
        'bpm_p95': quantile(bpms, .95),
        'bpm_spread_p90': quantile(bpms, .95) - quantile(bpms, .05) if bpms else None,
        'bpm_mae': statistics.mean(errors) if errors else None,
        'clock_bpm_mae': statistics.mean(clock_errors) if clock_errors else None,
        'clock_bpm_error_p95': quantile(clock_errors, .95),
        'first_lock_seconds': next((r['time'] for r in rows if r['estimate']['bpm'] is not None), None),
        'bpm_error_p95': quantile(errors, .95),
        'within_1_bpm_percent': 100 * sum(e <= 1 for e in errors) / evaluated if evaluated else None,
        'bpm_change_p99': quantile(changes, .99),
        'phase_jumps_over_40ms_per_min': jumps * 3000 / evaluated if evaluated else None,
        'absolute_phase_error_ms_p95': quantile(phases, .95),
    }


def load_rows(path):
    with path.open() as file:
        return [json.loads(line) for line in file]


def plot(identity, series, segments, duration):
    parts = [f'<h2>{html.escape(identity)}</h2><svg viewBox="0 0 1100 310" role="img" aria-label="Tempo over source time">']
    def x(t): return 50 + 1030 * t / duration
    def y(bpm): return 275 - min(260, max(0, bpm))
    for bpm in [50, 100, 150, 200, 250]:
        parts.append(f'<path d="M50 {y(bpm)}H1080" stroke="#363c45"/><text x="10" y="{y(bpm)+4}">{bpm}</text>')
    for segment in segments:
        parts.append(f'<path d="M{x(segment.get("start",0)):.1f} {y(segment["bpm"]):.1f}H{x(min(segment.get("end",duration),duration)):.1f}" stroke="white" stroke-dasharray="5 5"/>')
    for i, (name, rows) in enumerate(series):
        color = ['#70798a','#63c8c1','#9b749a','#ffbd59','#a9df78'][i % 5]
        points, previous = [], -1.
        for row in rows:
            bpm = row['estimate']['bpm']
            if bpm is not None and row['time'] - previous >= .25:
                points.append(f'{x(row["time"]):.1f},{y(bpm):.1f}')
                previous = row['time']
        parts.append(f'<polyline points="{" ".join(points)}" fill="none" stroke="{color}" stroke-width="1.2"/><text x="{55+i*205}" y="295" style="fill:{color}">{html.escape(name)}</text>')
    parts.append(f'<text x="1000" y="18">{duration:.1f} seconds</text></svg>')
    return ''.join(parts)


def valid_id(value):
    return isinstance(value, str) and bool(value) and all(c in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-' for c in value)


def positive(value, name, zero=False):
    if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value) or value < 0 or (not zero and value == 0):
        raise ValueError(f'{name} must be {"nonnegative" if zero else "positive"} and finite')
    return value


def case_specs(track, args):
    """Every case is a separate process; no detector or stabilizer state is reused."""
    if not args.cases:
        return [{'id': 'start', 'start': args.start, 'duration': args.limit,
                 'acquire_within': args.acquire_within}]
    cases = track.get('cases')
    if cases is None:
        fractions = args.midpoints if args.midpoints is not None else [.5]
        cases = [{'id': 'start', 'start': 0}] + [
            {'id': f'fraction-{fraction:g}'.replace('.', '_'), 'fraction': fraction}
            for fraction in fractions]
    if not isinstance(cases, list) or not cases:
        raise ValueError('cases must be a nonempty list')
    seen, result = set(), []
    for spec in cases:
        spec = dict(spec)
        identity = spec.get('id')
        if not valid_id(identity) or identity in seen:
            raise ValueError('case IDs must be unique within a track and contain only letters, digits, _ or -')
        seen.add(identity)
        if ('start' in spec) == ('fraction' in spec):
            raise ValueError('each case needs exactly one of start (seconds) or fraction (source duration)')
        if 'start' in spec:
            positive(spec['start'], 'case start', zero=True)
        else:
            positive(spec['fraction'], 'case fraction', zero=True)
            if spec['fraction'] >= 1:
                raise ValueError('case fraction must be less than 1')
        spec['duration'] = positive(spec.get('duration', args.case_duration), 'case duration')
        if args.limit is not None:
            spec['duration'] = min(spec['duration'], args.limit)
        spec['acquire_within'] = positive(spec.get('acquire_within', args.acquire_within), 'case acquire_within')
        result.append(spec)
    return result


def resolve_case(spec, source_duration):
    start = spec.get('start')
    if start is None:
        start = positive(source_duration, 'source duration') * spec['fraction']
    if source_duration is not None and start >= source_duration:
        raise ValueError(f'case {spec["id"]} starts at or beyond the end of the source')
    return start


def validate_cache(metadata, rate, model, start, requested_duration, cases):
    if metadata['sample_rate'] != rate:
        raise ValueError('cached sample rate differs from manifest')
    if metadata.get('model', 1) != model:
        raise ValueError('cached model differs; regenerate observations')
    if metadata.get('source_start_seconds', 0) != start:
        raise ValueError('cached source start differs; use the same --start or regenerate observations')
    # Legacy single-session --cached without --limit reuses the recorded span.
    # Cases always carry a duration, so changing one cannot silently reuse a trace.
    if cases or requested_duration is not None:
        if metadata.get('requested_duration_seconds') != requested_duration:
            raise ValueError('cached requested duration differs; regenerate observations')
    positive(metadata['duration'], 'cached duration')


def write_reports(directory, report, plots):
    (directory / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    with (directory / 'report.csv').open('w', newline='') as file:
        writer = csv.DictWriter(file, fieldnames=list(report[0]) if report else ['id'])
        writer.writeheader()
        writer.writerows(report)
    def display(value):
        return f'{value:.1f}' if isinstance(value, float) else '—' if value is None else str(value)
    headers = ['Track', 'Case', 'Source start', 'Duration', 'Backend / mode', 'Result',
               'First tempo', 'Correct lock starts', 'Correct lock confirmed', 'Correct after deadline',
               'Double tempo', 'Half tempo', 'Tolerance / sustain / deadline', 'Reason']
    table = []
    for result in report:
        values = [result['id'], result['case_id'], result['source_start_seconds'], result['duration_seconds'],
                  result['backend'] + ' / ' + result['mode'], result['status'], result['first_lock_seconds'],
                  result['first_correct_lock_seconds'], result['correct_lock_confirmed_seconds'],
                  result['correct_after_deadline_percent'], result['double_tempo_percent'], result['half_tempo_percent'],
                  f'±{result["bpm_tolerance"]:g} BPM / {result["sustain_seconds"]:g}s / {result["acquire_within_seconds"]:g}s',
                  result['failure_reason']]
        query = ' '.join(map(str, values))
        octave = any((result[key] or 0) > 0 for key in ['double_tempo_percent', 'half_tempo_percent'])
        slow = result['first_correct_lock_seconds'] is None or result['first_correct_lock_seconds'] > 10
        table.append(f'<tr data-octave="{int(octave)}" data-slow="{int(slow)}" data-status="{result["status"]}" data-search="{html.escape(query, quote=True)}">'
                     + ''.join('<td>' + html.escape(display(v)) + '</td>' for v in values) + '</tr>')
    (directory / 'report.html').write_text('''<!doctype html><meta charset="utf-8"><title>Tempo benchmark</title>
<style>body{background:#191b20;color:#ddd;font:14px system-ui;max-width:1600px;margin:40px auto;padding:0 16px}svg{width:100%;background:#22262d}text{fill:#bbc0c9}h2{margin-top:36px}.table-scroll{overflow-x:auto}table{width:100%;border-collapse:collapse;font-size:12px}th,td{text-align:left;padding:8px;border-bottom:1px solid #363c45}input,select{padding:8px;background:#22262d;color:#ddd;border:1px solid #666}tr[data-status="fail"] td:nth-child(6){color:#ffbd59}tr[data-status="unscored"] td:nth-child(6){color:#aaa}</style>
<h1>Offline tempo benchmark</h1><p>Every case starts a fresh tracker with no prefix audio. Times and lock latency are relative to that cold start. Source start and duration are seconds; coverage and octave columns are percentages. References may be approximate taps and never enter tracking. Correct lock requires both the displayed and output-clock tempo to remain within tolerance for the sustained interval. First tempo may be wrong. Correct lock starts at the beginning of a continuously correct interval; confirmation follows after the sustain interval. The scoring column lists the actual BPM tolerance, sustain interval and deadline for each row (defaults ±2 BPM / 5s / 30s). A pass also requires at least 90% correct coverage after its deadline. Early octave errors or acquisition over 10s can still pass, so use the problem filter to inspect them. An unscored case is not a pass. Cached replay preserves the original detector observations; run.json records their generation hashes separately from the current replay code (null means older traces without provenance).</p>
<p><input id="query" placeholder="Filter track, case, mode or reason"><select id="status"><option value="">All results</option><option>fail</option><option>unscored</option><option>pass</option></select> <select id="problem"><option value="">All problems</option><option value="octave">Octave errors (double / half)</option><option value="slow">Slow acquisition (&gt;10s or none)</option></select></p>
<div class="table-scroll"><table><thead><tr>''' + ''.join('<th>' + h + '</th>' for h in headers) + '</tr></thead><tbody>' + ''.join(table) + '</tbody></table></div>'
        + '<p>Dashed white: supplied reference. Traces: causal estimates sampled every 250 ms. Names and audio are not embedded. Phase accuracy requires annotated beat timestamps.</p>' + ''.join(plots) + '''
<script>const query=document.getElementById('query'),status=document.getElementById('status'),problem=document.getElementById('problem');function filter(){for(const row of document.querySelectorAll('tbody tr'))row.hidden=(!row.dataset.search.toLowerCase().includes(query.value.toLowerCase())||(status.value&&row.dataset.status!==status.value)||(problem.value&&row.dataset[problem.value]!=='1'));}query.addEventListener('input',filter);status.addEventListener('change',filter);problem.addEventListener('change',filter);</script>''')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--out', type=Path, default=ROOT / 'target' / 'benchmark')
    parser.add_argument('--backend', choices=['both', 'beatnet', 'pulseweave'], default='both')
    parser.add_argument('--start', type=float, default=0, help='Start a fresh tracker session this many seconds into each source')
    parser.add_argument('--limit', type=float, help='Decode at most this many seconds per session')
    parser.add_argument('--track', action='append', help='Manifest ID to include (repeatable)')
    parser.add_argument('--cases', action='store_true', help='Run each track\'s cases; absent cases use start and midpoint, each 90 seconds')
    parser.add_argument('--case', action='append', help='Case ID to include across selected tracks (repeatable; requires --cases)')
    parser.add_argument('--midpoints', type=float, action='append', help='Fallback source fraction when track has no cases (repeatable; default .5, plus start)')
    parser.add_argument('--case-duration', type=float, default=90., help='Default duration per case in seconds')
    parser.add_argument('--tolerance', type=float, default=2., help='BPM tolerance for approximate references (default 2)')
    parser.add_argument('--sustain', type=float, default=5., help='Continuous correct seconds required for lock (default 5)')
    parser.add_argument('--acquire-within', type=float, default=30., help='Correct-lock confirmation deadline in session seconds (default 30; case can override)')
    parser.add_argument('--check', action='store_true', help='Exit nonzero unless all selected assisted cases acquire by deadline and retain >=90%% correct tempo afterward')
    parser.add_argument('--model', type=int, choices=[1, 2, 3], default=1)
    parser.add_argument('--cached', action='store_true', help='Re-score existing raw traces without decoding/inference')
    args = parser.parse_args()
    try:
        execute(args, parser)
    except (ValueError, KeyError, TypeError, FileNotFoundError) as error:
        parser.error(str(error))


def execute(args, parser):
    manifest = json.loads(args.manifest.read_text())
    rate = manifest.get('sample_rate', 44100)
    positive(args.start, '--start', zero=True)
    for value, name in [(args.case_duration, '--case-duration'), (args.tolerance, '--tolerance'),
                        (args.sustain, '--sustain'), (args.acquire_within, '--acquire-within')]:
        positive(value, name)
    if args.limit is not None:
        positive(args.limit, '--limit')
    if args.cases and args.start:
        parser.error('--start cannot be combined with --cases; define case starts in the manifest')
    if (args.case or args.midpoints is not None) and not args.cases:
        parser.error('--case and --midpoints require --cases')
    if args.check and args.backend == 'pulseweave':
        parser.error('--check requires BeatNet assisted results; use --backend both')
    if not isinstance(rate, int) or not 8000 <= rate <= 192000:
        parser.error('sample_rate must be between 8000 and 192000')
    if args.track and set(args.track) - {t['id'] for t in manifest['tracks']}:
        parser.error('--track contains IDs not present in the manifest')
    ids, tracks, available_cases = set(), [], set()
    for track in manifest['tracks']:
        identity = track['id']
        if not valid_id(identity) or identity in ids:
            parser.error('track IDs must be unique, nonempty, and contain only letters, digits, _ or -')
        ids.add(identity)
        if args.track and identity not in args.track:
            continue
        last_end = -1
        for segment in track['segments']:
            positive(segment['bpm'], 'reference BPM')
            positive(segment.get('start', 0), 'segment start', zero=True)
            if 'end' in segment:
                positive(segment['end'], 'segment end')
                if segment['end'] <= segment.get('start', 0):
                    parser.error('ground-truth segment end must follow start')
            if 'beat_offset' in segment and not math.isfinite(segment['beat_offset']):
                parser.error('segment beat_offset must be finite')
            if segment.get('start', 0) < last_end:
                parser.error('ground-truth segments must be ordered and nonoverlapping')
            last_end = segment.get('end', math.inf)
        specs = case_specs(track, args)
        available_cases.update(spec['id'] for spec in specs)
        tracks.append((track, specs))
    if args.case and set(args.case) - available_cases:
        parser.error('--case contains IDs not present in selected tracks')
    selected = [(track, [spec for spec in specs if not args.case or spec['id'] in args.case]) for track, specs in tracks]
    if not any(specs for _, specs in selected):
        parser.error('no tracks or cases selected')
    args.out.mkdir(parents=True, exist_ok=True)
    run_info = {'complete': False, 'sample_rate': rate, 'model': args.model, 'cases': args.cases,
                'source_start_seconds': args.start, 'tolerance': args.tolerance, 'sustain': args.sustain,
                'acquire_within': args.acquire_within,
                'manifest_sha256': hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
                'source_sha256': {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                                  for name in ['Cargo.lock', 'src/backend.rs', 'src/clock.rs', 'src/config.rs',
                                               'tools/replay.rs', 'tools/benchmark.py']},
                'observation_provenance': {}}
    (args.out / 'run.json').write_text(json.dumps(run_info, indent=2) + '\n')
    subprocess.run(['cargo', 'build', '--release', '--no-default-features', '--features', 'offline', '--bin', 'track-replay'], cwd=ROOT, check=True)
    report, plots, missing_assisted = [], [], []
    for track, specs in selected:
        if not specs:
            continue
        identity = track['id']
        source = (args.manifest.resolve().parent / track['file']).resolve()
        source_duration = source_hash = channels = None
        if not args.cached:
            probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-select_streams', 'a:0', '-show_entries', 'stream=channels,duration:format=duration', '-of', 'json', str(source)]))
            channels = probe['streams'][0]['channels']
            raw_duration = probe['streams'][0].get('duration', probe.get('format', {}).get('duration'))
            source_duration = float(raw_duration) if raw_duration not in (None, 'N/A') else None
            digest = hashlib.sha256()
            with source.open('rb') as file:
                while block := file.read(1024 * 1024):
                    digest.update(block)
            source_hash = digest.hexdigest()
        for spec in specs:
            directory = args.out / identity / spec['id'] if args.cases else args.out
            directory.mkdir(parents=True, exist_ok=True)
            meta = directory / f'{identity}.meta.json'
            metadata = json.loads(meta.read_text()) if args.cached else None
            recorded_source_duration = metadata.get('source_duration_seconds') if metadata else source_duration
            start = resolve_case(spec, recorded_source_duration)
            requested_duration = spec['duration']
            segments = session_segments(track['segments'], start)
            with tempfile.TemporaryDirectory(prefix='tempotrack-', dir=directory) as temporary:
                pcm = Path(temporary) / 'audio.f32'
                if not args.cached:
                    command = ['ffmpeg', '-nostdin', '-v', 'error']
                    if start:
                        # Accurate transcoding seek; prefix audio never reaches tracking.
                        command += ['-ss', str(start)]
                    command += ['-i', str(source)]
                    if requested_duration is not None:
                        command += ['-t', str(requested_duration)]
                    mix = '+'.join(f'{1/channels}*c{i}' for i in range(channels))
                    command += ['-map', '0:a:0', '-af', f'pan=mono|c0={mix}', '-ar', str(rate), '-f', 'f32le', str(pcm)]
                    subprocess.run(command, check=True)
                    duration = pcm.stat().st_size / 4 / rate
                    if duration <= 0:
                        parser.error(f'no audio after source start for {identity}/{spec["id"]}')
                    metadata = {'duration': duration, 'sample_rate': rate, 'source_sha256': source_hash,
                                'trace_version': 1, 'model': args.model, 'source_start_seconds': start,
                                'requested_duration_seconds': requested_duration,
                                'source_duration_seconds': source_duration,
                                'generation_source_sha256': run_info['source_sha256']}
                    meta.write_text(json.dumps(metadata))
                else:
                    validate_cache(metadata, rate, args.model, start, requested_duration, args.cases)
                    duration = metadata['duration']
                run_info['observation_provenance'][f'{identity}/{spec["id"]}'] = {
                    'source_sha256': metadata['source_sha256'],
                    'generation_source_sha256': metadata.get('generation_source_sha256'),
                    'cached': args.cached,
                }
                series = []
                case_report = []
                backends = ['pulseweave', 'beatnet'] if args.backend == 'both' else [args.backend]
                for backend in backends:
                    trace = directory / f'{identity}.{backend}.raw.jsonl'
                    if not args.cached:
                        print(f'Processing {identity}/{spec["id"]} / {backend} (source {start:.1f}s, duration {duration:.1f}s)', flush=True)
                        with trace.open('w') as output:
                            subprocess.run([str(ROOT / 'target/release/track-replay'), '--pcm', str(pcm), '--rate', str(rate), '--tracking', backend, '--model', str(args.model)], stdout=output, check=True)
                        trace.with_suffix('.meta.json').write_text(json.dumps(metadata))
                    elif json.loads(trace.with_suffix('.meta.json').read_text()) != metadata:
                        parser.error(f'cache metadata differs for {identity}/{spec["id"]}/{backend}; regenerate observations')
                    modes = ['raw', 'stable']
                    guide = directory / f'{identity}.pulseweave.raw.jsonl'
                    guide_meta = guide.with_suffix('.meta.json')
                    if backend == 'beatnet' and guide.exists() and guide_meta.exists():
                        if json.loads(guide_meta.read_text()) == metadata:
                            modes.append('assisted')
                        else:
                            print(f'Skipping advisor for {identity}/{spec["id"]}: cached detector inputs differ', flush=True)
                    for mode in modes:
                        measured = trace
                        if mode != 'raw':
                            measured = directory / f'{identity}.{backend}.{mode}.jsonl'
                            command = [str(ROOT / 'target/release/track-replay'), '--replay', str(trace), '--tracking', backend, '--model', str(args.model)]
                            if mode == 'assisted':
                                command += ['--guide', str(guide)]
                            with measured.open('w') as output:
                                subprocess.run(command, stdout=output, check=True)
                        rows = load_rows(measured)
                        result = {'id': identity, 'case_id': spec['id'], 'reference': track.get('reference', 'tempo_only'),
                                  'backend': backend, 'mode': mode, 'model': args.model,
                                  'source_start_seconds': start, 'duration_seconds': duration,
                                  **metrics(rows, segments, duration, args.tolerance, args.sustain, spec['acquire_within'])}
                        series.append((f'{backend} {mode}', rows))
                        case_report.append(result)
                        report.append(result)
                        print(json.dumps(result), flush=True)
                        (args.out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
                if not any(r['mode'] == 'assisted' for r in case_report):
                    missing_assisted.append(f'{identity}/{spec["id"]}')
                label = f'{identity} / {spec["id"]} — source {start:g}s'
                plots.append(plot(label, series, segments, duration))
    write_reports(args.out, report, plots)
    assisted = [r for r in report if r['mode'] == 'assisted']
    run_info.update(complete=True, assisted_status_counts={status: sum(r['status'] == status for r in assisted)
                                                         for status in ['pass', 'fail', 'unscored']},
                    missing_assisted=missing_assisted)
    (args.out / 'run.json').write_text(json.dumps(run_info, indent=2) + '\n')
    if args.check and (missing_assisted or not assisted or any(r['status'] != 'pass' for r in assisted)):
        print('Check failed: assisted cases failed, are unscored, or lack a matching advisor trace.', flush=True)
        raise SystemExit(1)


if __name__ == '__main__':
    main()
