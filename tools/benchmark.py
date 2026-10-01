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


def metrics(rows, segments, duration):
    times = [r['time'] for r in rows]
    errors, bpms, phases, changes, clock_errors = [], [], [], [], []
    evaluated = 0
    holding = 0
    jumps = 0
    for segment in segments:
        start, end = segment.get('start', 0), min(segment.get('end', duration), duration)
        previous = None
        for tick in range(math.ceil(start * 50), math.floor(end * 50)):
            t = tick / 50
            evaluated += 1
            index = bisect.bisect_right(times, t) - 1
            if index < 0:
                continue
            estimate = rows[index]['estimate']
            grid = estimate['grids'][1]
            if grid is None or estimate['bpm'] is None:
                continue
            holding += bool(estimate.get('holding', False))
            bpm = estimate['bpm']
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
    return {
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--out', type=Path, default=ROOT / 'target' / 'benchmark')
    parser.add_argument('--backend', choices=['both', 'beatnet', 'pulseweave'], default='both')
    parser.add_argument('--start', type=float, default=0, help='Start a fresh tracker session this many seconds into each source')
    parser.add_argument('--limit', type=float, help='Decode only this many seconds after --start')
    parser.add_argument('--track', action='append', help='Manifest ID to include (repeatable)')
    parser.add_argument('--model', type=int, choices=[1, 2, 3], default=1)
    parser.add_argument('--cached', action='store_true', help='Re-score existing raw traces without decoding/inference')
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text())
    rate = manifest.get('sample_rate', 44100)
    if not math.isfinite(args.start) or args.start < 0:
        parser.error('--start must be nonnegative and finite')
    if args.limit is not None and (not math.isfinite(args.limit) or args.limit <= 0):
        parser.error('--limit must be positive and finite')
    if not isinstance(rate, int) or not 8000 <= rate <= 192000:
        parser.error('sample_rate must be between 8000 and 192000')
    if args.track and set(args.track) - {t['id'] for t in manifest['tracks']}:
        parser.error('--track contains IDs not present in the manifest')
    args.out.mkdir(parents=True, exist_ok=True)
    run_info = {'complete': False, 'sample_rate': rate, 'model': args.model,
                'source_start_seconds': args.start,
                'manifest_sha256': hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
                'source_sha256': {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                                  for name in ['Cargo.lock', 'src/backend.rs', 'src/clock.rs', 'src/config.rs']}}
    (args.out / 'run.json').write_text(json.dumps(run_info, indent=2) + '\n')
    subprocess.run(['cargo', 'build', '--release', '--no-default-features', '--features', 'offline', '--bin', 'track-replay'], cwd=ROOT, check=True)
    report = []
    plots = []
    ids = set()
    for track in manifest['tracks']:
        identity = track['id']
        if not identity or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-' for c in identity) or identity in ids:
            parser.error('track IDs must be unique, nonempty, and contain only letters, digits, _ or -')
        ids.add(identity)
        if args.track and identity not in args.track:
            continue
        segments = track['segments']
        last_end = -1
        for segment in segments:
            if not math.isfinite(segment['bpm']) or segment['bpm'] <= 0 or segment.get('start', 0) < 0 or segment.get('end', math.inf) <= segment.get('start', 0):
                parser.error('invalid ground-truth segment')
            if not math.isfinite(segment.get('start', 0)) or ('end' in segment and not math.isfinite(segment['end'])) or ('beat_offset' in segment and not math.isfinite(segment['beat_offset'])):
                parser.error('segment timestamps must be finite')
            if segment.get('start', 0) < last_end:
                parser.error('ground-truth segments must be ordered and nonoverlapping')
            last_end = segment.get('end', math.inf)
        segments = session_segments(segments, args.start)
        backends = ['pulseweave', 'beatnet'] if args.backend == 'both' else [args.backend]
        with tempfile.TemporaryDirectory(prefix='tempotrack-', dir=args.out) as temporary:
            pcm = Path(temporary) / 'audio.f32'
            meta = args.out / f'{identity}.meta.json'
            if not args.cached:
                source = (args.manifest.resolve().parent / track['file']).resolve()
                command = ['ffmpeg', '-nostdin', '-v', 'error']
                if args.start:
                    # Input seeking is sample-accurate when transcoding. No prefix
                    # audio reaches the tracker, so this is a true cold start.
                    command += ['-ss', str(args.start)]
                command += ['-i', str(source)]
                if args.limit:
                    command += ['-t', str(args.limit)]
                # Explicit equal-weight mixing matches the live capture path.
                probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-select_streams', 'a:0', '-show_entries', 'stream=channels', '-of', 'json', str(source)]))
                channels = probe['streams'][0]['channels']
                mix = '+'.join(f'{1/channels}*c{i}' for i in range(channels))
                command += ['-map', '0:a:0', '-af', f'pan=mono|c0={mix}', '-ar', str(rate), '-f', 'f32le', str(pcm)]
                subprocess.run(command, check=True)
                duration = pcm.stat().st_size / 4 / rate
                if duration <= 0:
                    parser.error(f'no audio after --start for {identity}')
                digest = hashlib.sha256()
                with source.open('rb') as file:
                    while block := file.read(1024 * 1024):
                        digest.update(block)
                metadata = {'duration': duration, 'sample_rate': rate, 'source_sha256': digest.hexdigest(), 'trace_version': 1, 'model': args.model,
                            'source_start_seconds': args.start}
                meta.write_text(json.dumps(metadata))
            else:
                metadata = json.loads(meta.read_text())
                if args.limit is not None:
                    parser.error('--limit cannot be used with --cached; cached traces already have a fixed duration')
                if metadata['sample_rate'] != rate:
                    parser.error('cached sample rate differs from manifest')
                if metadata.get('model', 1) != args.model:
                    parser.error('cached model differs; regenerate observations')
                if metadata.get('source_start_seconds', 0) != args.start:
                    parser.error('cached source start differs; use the same --start or regenerate observations')
                duration = metadata['duration']
            series = []
            for backend in backends:
                trace = args.out / f'{identity}.{backend}.raw.jsonl'
                if not args.cached:
                    print(f'Processing {identity} / {backend} ({duration:.1f}s)', flush=True)
                    with trace.open('w') as output:
                        subprocess.run([str(ROOT / 'target/release/track-replay'), '--pcm', str(pcm), '--rate', str(rate), '--tracking', backend, '--model', str(args.model)], stdout=output, check=True)
                    trace.with_suffix('.meta.json').write_text(json.dumps(metadata))
                elif json.loads(trace.with_suffix('.meta.json').read_text()) != metadata:
                    parser.error(f'cache metadata differs for {identity}/{backend}; regenerate observations')
                modes = ['raw', 'stable']
                guide = args.out / f'{identity}.pulseweave.raw.jsonl'
                guide_meta = guide.with_suffix('.meta.json')
                if backend == 'beatnet' and guide.exists() and guide_meta.exists():
                    if json.loads(guide_meta.read_text()) == metadata:
                        modes.append('assisted')
                    else:
                        print(f'Skipping advisor for {identity}: cached detector inputs differ', flush=True)
                for mode in modes:
                    measured = trace
                    if mode != 'raw':
                        measured = args.out / f'{identity}.{backend}.{mode}.jsonl'
                        command = [str(ROOT / 'target/release/track-replay'), '--replay', str(trace), '--tracking', backend, '--model', str(args.model)]
                        if mode == 'assisted':
                            command += ['--guide', str(args.out / f'{identity}.pulseweave.raw.jsonl')]
                        with measured.open('w') as output:
                            subprocess.run(command, stdout=output, check=True)
                    rows = load_rows(measured)
                    result = {'id': identity, 'reference': track.get('reference', 'tempo_only'), 'backend': backend, 'mode': mode, 'model': args.model,
                              'source_start_seconds': args.start, **metrics(rows, segments, duration)}
                    series.append((f'{backend} {mode}', rows))
                    report.append(result)
                    print(json.dumps(result), flush=True)
                    (args.out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            plots.append(plot(identity, series, segments, duration))
    (args.out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    with (args.out / 'report.csv').open('w', newline='') as file:
        writer = csv.DictWriter(file, fieldnames=list(report[0]) if report else ['id'])
        writer.writeheader()
        writer.writerows(report)
    (args.out / 'report.html').write_text('<!doctype html><meta charset="utf-8"><title>Tempo benchmark</title><style>body{background:#191b20;color:#ddd;font:14px system-ui;max-width:1150px;margin:40px auto}svg{width:100%;background:#22262d}text{fill:#bbc0c9}h2{margin-top:36px}</style><h1>Offline tempo benchmark</h1><p>Dashed white: supplied reference (possibly approximate/tapped). Distance from this line is not proof of tracking error. Traces: causal estimates sampled every 250 ms. Names and audio are not embedded. Phase accuracy requires annotated beat timestamps.</p>'
        + f'<p>Times are relative to a fresh tracker session, starting {args.start:g} seconds into each source. Reference windows and beat timestamps are shifted by the same offset. First lock includes acquisition from this cold start.</p>' + ''.join(plots))

    run_info['complete'] = True
    (args.out / 'run.json').write_text(json.dumps(run_info, indent=2) + '\n')


if __name__ == '__main__':
    main()
