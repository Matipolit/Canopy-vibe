"""Octave-band energy of the first 0.5 s of notes, dB relative to the
note's total: bands.py LIST (label path midi [start])"""
import sys, os, math, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gtr
EDGES = [0, 250, 500, 1000, 2000, 4000, 8000, 16000]
rows = {}
for line in open(sys.argv[1]):
    p = line.rstrip('\n').split('\t')
    label, path, midi = p[0], p[1], int(p[2])
    x, sr = gtr.load(path, float(p[3]) if len(p) > 3 else 0.0)
    t0 = gtr.onset(x, sr)
    seg = x[t0:t0 + int(0.5 * sr)]
    S, df = gtr.spectrum(seg, sr, pad=1)
    fr = np.arange(len(S)) * df
    tot = (S[fr > 60] ** 2).sum()
    e = [10 * math.log10((S[(fr >= a) & (fr < b) & (fr > 60)] ** 2).sum() / tot + 1e-12) for a, b in zip(EDGES, EDGES[1:])]
    rows.setdefault(label, {})[midi] = e
labels = list(rows)
print('band(Hz)   ' + '  '.join(f'{a}-{b}' for a, b in zip(EDGES, EDGES[1:])))
for l in labels:
    for m, e in sorted(rows[l].items()):
        print(f'{l[:16]:16} {m:3d} ' + ' '.join(f'{v:7.1f}' for v in e))
    avg = np.mean(list(rows[l].values()), axis=0)
    print(f'{l[:16]:16} avg ' + ' '.join(f'{v:7.1f}' for v in avg))
