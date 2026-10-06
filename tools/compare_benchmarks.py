#!/usr/bin/env python3
"""Compare two offline benchmark directories without reopening private audio."""
import argparse
import html
import json
import math
from pathlib import Path
import re
import statistics


IDENTITY = ('id', 'case_id', 'backend', 'mode')
SCORING = ('reference', 'model', 'source_start_seconds', 'duration_seconds',
           'bpm_tolerance', 'sustain_seconds', 'acquire_within_seconds')
METRICS = (
    ('first_lock_seconds', 'First tempo · s', -1, .5),
    ('correct_lock_confirmed_seconds', 'Correct lock confirmed · s', -1, .5),
    ('correct_after_deadline_percent', 'Correct after deadline · %', 1, 1.),
    ('double_tempo_percent', 'Double tempo · %', -1, 1.),
    ('half_tempo_percent', 'Half tempo · %', -1, 1.),
    ('clock_bpm_mae', 'Clock error · BPM', -1, .25),
    ('phase_jumps_over_40ms_per_min', 'Phase jumps >40ms · /min', -1, 1.),
)


def read_json(path):
    with path.open() as file:
        return json.load(file)


def load_run(directory):
    run = read_json(directory / 'run.json')
    if run.get('complete') is not True:
        raise ValueError(f'{directory}: benchmark is incomplete; finish the run first')
    rows = {}
    for row in read_json(directory / 'report.json'):
        key = tuple(row[field] for field in IDENTITY)
        if any(not isinstance(part, str) or not re.fullmatch(r'[A-Za-z0-9_-]+', part)
               for part in key):
            raise ValueError('Report identities must be safe track/case/backend/mode IDs')
        if key in rows:
            raise ValueError(f'Duplicate report identity: {key}')
        rows[key] = row
    return run, rows


def trace_directory(directory, run, key):
    return directory / key[0] / key[1] if run.get('cases') else directory


def compatibility(before, after, old_run, new_run, old_rows, new_rows):
    """Refuse silent comparisons across changed audio, annotations, or scoring."""
    problems = []
    for field in ('manifest_sha256', 'sample_rate', 'model'):
        if old_run.get(field) is None or old_run.get(field) != new_run.get(field):
            problems.append(f'Run {field} differs or is missing')
    missing = set(old_rows) ^ set(new_rows)
    if missing:
        problems.append(f'{len(missing)} report identities are missing from one run')
    checked = set()
    for key in sorted(set(old_rows) & set(new_rows)):
        label = '/'.join(key)
        for field in SCORING:
            if field not in old_rows[key] or old_rows[key][field] != new_rows[key].get(field):
                problems.append(f'{label}: {field} differs or is missing')
        if key[:2] in checked:
            continue
        checked.add(key[:2])
        metadata = [read_json(trace_directory(path, run, key) / f'{key[0]}.meta.json')
                    for path, run in [(before, old_run), (after, new_run)]]
        for field in ('source_sha256', 'sample_rate', 'model', 'source_start_seconds', 'duration'):
            if metadata[0].get(field) is None or metadata[0].get(field) != metadata[1].get(field):
                problems.append(f'{key[0]}/{key[1]}: source {field} differs or is missing')
    return problems


def load_trace(path):
    with path.open() as file:
        rows = [json.loads(line) for line in file if line.strip()]
    previous = -math.inf
    for row in rows:
        time = row['time']
        if not math.isfinite(time) or time < previous:
            raise ValueError(f'{path.name}: trace timestamps must be finite and ordered')
        previous = time
    return rows


def series(rows, clock=False):
    """None explicitly breaks a line when the tracker has no estimate."""
    points = []
    for row in rows:
        estimate = row['estimate']
        grid = estimate['grids'][1]
        value = 60 / grid['period'] if clock and grid and grid['period'] > 0 else None
        if not clock:
            value = estimate.get('bpm')
        points.append((row['time'], value if value is not None and math.isfinite(value) else None))
    return points


def paths(points, duration, low, high):
    """Retain every observation; only remove points identical at SVG precision."""
    result, current, previous = [], [], None
    for time, bpm in points:
        if bpm is None:
            if current:
                result.append(' '.join(current))
            current, previous = [], None
            continue
        point = f'{64 + time / duration * 910:.2f},{170 - (bpm - low) / (high - low) * 142:.2f}'
        if point != previous:
            current.append(('M' if not current else 'L') + point)
            previous = point
    if current:
        result.append(' '.join(current))
    return result


def plot(old, new, duration, clock=False):
    values = [series(old, clock), series(new, clock)]
    tempos = [value for points in values for _, value in points if value is not None]
    low = math.floor((min(tempos, default=100) - 3) / 10) * 10
    high = math.ceil((max(tempos, default=140) + 3) / 10) * 10
    title = 'Actual output clock' if clock else 'Displayed tempo'
    parts = ['<svg viewBox="0 0 1000 205">',
             f'<text x="64" y="16">{title} · BPM</text>']
    for index in range(5):
        y = 170 - index * 142 / 4
        value = low + index * (high - low) / 4
        parts += [f'<path class="grid" d="M64,{y}H974"/>',
                  f'<text x="55" y="{y + 4}" text-anchor="end">{value:g}</text>']
    for index in range(7):
        x, time = 64 + index * 910 / 6, duration * index / 6
        parts.append(f'<text x="{x}" y="192" text-anchor="middle">{time:.0f}s</text>')
    for points, color in zip(values, ['before', 'after']):
        for path in paths(points, duration, low, high):
            parts.append(f'<path class="line {color}" d="{path}"/>')
    parts.append('</svg>')
    return ''.join(parts)


def number(value):
    return '—' if value is None else f'{value:.2f}'


def metric_change(old, new, direction, threshold):
    if old is None or new is None:
        return 'unavailable'
    signed = (new - old) * direction
    return 'regression' if signed < -threshold else 'improvement' if signed > threshold else 'similar'


def render(before, after, allow_mismatch=False):
    old_run, old_rows = load_run(before)
    new_run, new_rows = load_run(after)
    problems = compatibility(before, after, old_run, new_run, old_rows, new_rows)
    if problems and not allow_mismatch:
        raise ValueError('Incompatible benchmarks:\n' + '\n'.join(problems)
                         + '\nUse identical inputs/scoring, or --allow-mismatch to render a clearly marked exploratory comparison.')
    keys = sorted(key for key in set(old_rows) & set(new_rows) if key[3] == 'assisted')
    if not keys:
        raise ValueError('No matching assisted cases; run the benchmark with both detectors first')
    summary = []
    for field, label, _, _ in METRICS:
        pairs = [(old_rows[key].get(field), new_rows[key].get(field)) for key in keys]
        pairs = [(a, b) for a, b in pairs if a is not None and b is not None]
        a = statistics.mean(pair[0] for pair in pairs) if pairs else None
        b = statistics.mean(pair[1] for pair in pairs) if pairs else None
        delta = '—' if a is None or b is None else f'{b - a:+.2f}'
        summary.append(f'<tr><td>{label}</td><td>{number(a)}</td><td>{number(b)}</td><td>{delta}</td><td>{len(pairs)}</td></tr>')
    cards, counts = [], {'pass → fail': 0, 'fail → pass': 0, 'same status': 0, 'other status change': 0}
    for key in keys:
        old, new = old_rows[key], new_rows[key]
        transition = f'{old["status"]} → {new["status"]}'
        count_key = transition if transition in counts else 'same status' if old['status'] == new['status'] else 'other status change'
        counts[count_key] += 1
        metric_rows, flags = [], []
        for field, label, direction, threshold in METRICS:
            a, b = old.get(field), new.get(field)
            change = metric_change(a, b, direction, threshold)
            if field.endswith('_seconds') and a is not None and b is None:
                change = 'regression'
            if change == 'regression':
                flags.append(label)
            delta = '—' if a is None or b is None else f'{b - a:+.2f}'
            metric_rows.append(f'<tr class="{change}"><td>{label}</td><td>{number(a)}</td>'
                               f'<td>{number(b)}</td><td>{delta}</td></tr>')
        if old['status'] == 'pass' and new['status'] != 'pass':
            flags.insert(0, 'status')
        traces = []
        for directory, run in [(before, old_run), (after, new_run)]:
            path = trace_directory(directory, run, key) / f'{key[0]}.{key[2]}.{key[3]}.jsonl'
            traces.append(load_trace(path))
        label = html.escape(' / '.join(key))
        scoring = ' · '.join(f'{field}: {html.escape(str(new[field]))}' for field in SCORING)
        reasons = '<br>'.join(f'{side}: {html.escape(row.get("failure_reason") or row["status"])}'
                              for side, row in [('Before', old), ('After', new)])
        cards.append(f'<section data-regression="{int(bool(flags))}" data-fail="{int(new["status"] != "pass")}">'
                     f'<h2>{label} <span>{html.escape(transition)}</span></h2><p class="meta">{scoring}</p>'
                     f'<p>{reasons}</p><p class="flags">Metric regressions: {html.escape(", ".join(flags) or "none")}</p>'
                     '<table><thead><tr><th>Metric</th><th>Before</th><th>After</th><th>Δ after − before</th></tr></thead>'
                     '<tbody>' + ''.join(metric_rows) + '</tbody></table>'
                     + ''.join(plot(*traces, max(old['duration_seconds'], new['duration_seconds']), clock)
                               for clock in (False, True)) + '</section>')
    warning = ('<aside><strong>EXPLORATORY — INPUTS OR SCORING DIFFER</strong><ul>'
               + ''.join(f'<li>{html.escape(problem)}</li>' for problem in problems) + '</ul></aside>') if problems else ''
    provenance = json.dumps({'before': old_run, 'after': new_run}, indent=2)
    return '''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Tempo tracking comparison</title><style>
body{font:15px system-ui;background:#131719;color:#e7e9e8;margin:32px auto;max-width:1100px;padding:0 20px}
h1,h2{font-weight:550}h2{font-size:19px}h2 span{float:right;color:#b5c2c6}p{line-height:1.5}.meta{font-size:12px;color:#a9b1b4}
section{padding:22px;margin:24px 0;background:#1d2326;border:1px solid #354047;border-radius:12px}
table{width:100%;border-collapse:collapse;font-size:13px}th,td{text-align:right;padding:7px;border-bottom:1px solid #354047}
th:first-child,td:first-child{text-align:left}.regression td:last-child,.flags{color:#edb58e}.improvement td:last-child{color:#82d4bd}
svg{display:block;width:100%;margin-top:20px}svg text{fill:#a9b1b4;font:12px system-ui}.grid{stroke:#354047;stroke-width:1}
.line{fill:none;stroke-width:1.8;stroke-linejoin:round;opacity:.85}.before{stroke:#d6a56a}.after{stroke:#76d2cb}
.legend-before{color:#d6a56a}.legend-after{color:#76d2cb}aside{border:2px solid #edb58e;padding:16px}
select,input{background:#242c30;color:#e7e9e8;border:1px solid #657077;padding:8px;margin-right:12px}pre{overflow:auto;font-size:11px}
</style><h1>Tempo tracking · before / after</h1>''' + warning + f'<p>{len(keys)} matching assisted cases · ' + ' · '.join(
        f'{label}: {count}' for label, count in counts.items()) + '''</p>
<p>Summary means give every case equal weight and include only cases with the metric present in both runs.
The paired-case count exposes missing locks; status transitions and case flags still include those cases.</p>
<table><thead><tr><th>Metric · mean across paired cases</th><th>Before</th><th>After</th><th>Δ</th><th>Paired cases</th></tr></thead><tbody>
''' + ''.join(summary) + '''</tbody></table>
<p><span class="legend-before">Before: amber</span> · <span class="legend-after">After: teal</span>.
Each case has separate displayed-tempo and actual-clock plots; they use their own BPM scale. Times are seconds after the cold start.
No reference line is inferred from an estimate: reports contain scores but not reference beat timelines. All trace observations are retained.
Blank intervals mean no estimate. This comparison does not measure absolute beat phase without annotated beats.</p>
<p>Regression flags indicate worse metrics, even when status still passes: over 0.5s lock latency, 1 percentage point of coverage/octave error,
0.25 BPM clock error, or 1 phase jump/minute. Missing metrics show “—”; they are not treated as zero.</p>
<input id="search" placeholder="Filter track / case"><select id="filter"><option value="all">All cases</option>
<option value="regression">Any metric regression</option><option value="fail">Still failing / unscored</option></select>
''' + ''.join(cards) + '<details><summary>Run provenance and scoring</summary><pre>' + html.escape(provenance) + '''</pre></details>
<script>function filter(){const q=document.getElementById('search').value.toLowerCase(),mode=document.getElementById('filter').value;
for(const card of document.querySelectorAll('section'))card.hidden=!card.querySelector('h2').textContent.toLowerCase().includes(q)||(mode!=='all'&&card.dataset[mode]!=='1');}
document.getElementById('search').addEventListener('input',filter);document.getElementById('filter').addEventListener('change',filter);</script></html>'''


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('before', type=Path)
    parser.add_argument('after', type=Path)
    parser.add_argument('--out', type=Path, required=True, help='Standalone HTML destination')
    parser.add_argument('--allow-mismatch', action='store_true', help='Mark incompatible inputs/scoring as exploratory')
    args = parser.parse_args(argv)
    try:
        document = render(args.before, args.after, args.allow_mismatch)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.error(str(error))
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(document)
    print(args.out)


if __name__ == '__main__':
    main()
