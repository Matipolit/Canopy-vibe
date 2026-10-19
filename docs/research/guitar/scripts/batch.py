#!/usr/bin/env python3
"""Condensed measurements of many notes: batch.py LIST.tsv > out.tsv

LIST rows: label <tab> path <tab> midi [<tab> start <tab> length]
"""
import json, statistics, sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gtr


def med(values):
    v = [x for x in values if x is not None and x != float('inf') and x > 0]
    return round(statistics.median(v), 2) if v else None


def condense(d):
    rows = d['per_partial']
    if not rows:
        return {}
    top = max(r['level'] for r in rows)
    strong = [r for r in rows if r['level'] > top - 25]
    low = [r for r in strong if r['n'] <= 4]
    mid = [r for r in strong if 5 <= r['n'] <= 12]
    rms = d['rms_db_per_s']
    slope = (rms[min(9, len(rms) - 1)] - rms[0]) / (0.2 * min(9, len(rms) - 1)) if len(rms) > 2 else None
    return {
        'B': '%.2e' % d['B'], 'cents': d['cents'],
        't60_low_early': med(r['early'] for r in low), 't60_low_late': med(r['late'] for r in low),
        't60_mid_early': med(r['early'] for r in mid), 't60_mid_late': med(r['late'] for r in mid),
        'beat_low': med(r['beat_db'] for r in low), 'beat_mid': med(r['beat_db'] for r in mid),
        'rms_db_s': round(slope, 1) if slope is not None else None,
        'cA': d['centroid_attack'], 'c300': d['centroid_300ms'], 'noise': d['attack_noise'],
        'glide': ','.join(str(v) for v in d.get('glide_cents', [])),
    }


KEYS = ['B', 'cents', 't60_low_early', 't60_low_late', 't60_mid_early', 't60_mid_late',
        'beat_low', 'beat_mid', 'rms_db_s', 'cA', 'c300', 'noise', 'glide']

print('\t'.join(['label', 'midi'] + KEYS))
for line in open(sys.argv[1]):
    if not line.strip() or line.startswith('#'):
        continue
    parts = line.rstrip('\n').split('\t')
    label, path, midi = parts[0], parts[1], int(parts[2])
    start = float(parts[3]) if len(parts) > 3 else 0.0
    length = float(parts[4]) if len(parts) > 4 else None
    try:
        c = condense(gtr.note(path, midi, start, length))
    except Exception as e:
        print(label, 'ERROR', e, file=sys.stderr)
        continue
    print('\t'.join([label, str(midi)] + [str(c.get(k)) for k in KEYS]), flush=True)
