#!/usr/bin/env python3
"""Measurements of plucked-string recordings, the same for real guitars and
for renders of the model.

    gtr.py note FILE MIDI [--start S] [--len L]   single-note report (JSON line)
    gtr.py ltas FILE [--start S] [--len L]        1/6-octave long-term spectrum

Audio is decoded with ffmpeg to mono float32 (files are untrusted data and
are only ever read as audio).
"""
import json, math, subprocess, sys
import numpy as np


def load(path, start=0.0, length=None):
    sr = int(subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'a:0',
                             '-show_entries', 'stream=sample_rate', '-of', 'csv=p=0', path],
                            capture_output=True, text=True, check=True).stdout.strip().split(',')[0])
    cmd = ['ffmpeg', '-v', 'error', '-ss', str(start)]
    if length:
        cmd += ['-t', str(length)]
    cmd += ['-i', path, '-f', 'f32le', '-ac', '1', '-']
    raw = subprocess.run(cmd, capture_output=True, check=True).stdout
    return np.frombuffer(raw, dtype=np.float32).astype(np.float64), sr


def onset(x, sr):
    env = np.abs(x)
    k = max(1, int(0.002 * sr))
    env = np.convolve(env, np.ones(k) / k, mode='same')
    peak = env.max()
    i = int(np.argmax(env > 0.1 * peak))
    return max(0, i - int(0.003 * sr))


def spectrum(seg, sr, pad=4):
    n = len(seg)
    w = np.hanning(n)
    N = 1 << int(math.ceil(math.log2(n * pad)))
    S = np.abs(np.fft.rfft(seg * w, N))
    return S, sr / N


def peak_near(S, df, f, half):
    lo = max(1, int((f - half) / df))
    hi = min(len(S) - 2, int((f + half) / df) + 1)
    if hi <= lo:
        return None
    i = lo + int(np.argmax(S[lo:hi]))
    a, b, c = np.log(S[i - 1] + 1e-30), np.log(S[i] + 1e-30), np.log(S[i + 1] + 1e-30)
    d = 0.5 * (a - c) / (a - 2 * b + c) if (a - 2 * b + c) != 0 else 0.0
    return (i + d) * df, S[i]


def partials(x, sr, f_guess, t0, length=1.0, max_hz=3000.0, nmax=30):
    seg = x[t0 + int(0.05 * sr): t0 + int((0.05 + length) * sr)]
    S, df = spectrum(seg, sr)
    floor = np.median(S) + 1e-30
    # The fundamental may be weak (a body or pickup) : find f0 from partials 1..4.
    best = None
    for cents in np.linspace(-60, 60, 121):
        f = f_guess * 2 ** (cents / 1200)
        score = sum(np.log(peak_near(S, df, k * f, 0.02 * k * f)[1] + 1e-30) for k in range(1, 5))
        if best is None or score > best[0]:
            best = (score, f)
    f0 = best[1]
    found = []
    B = 0.0
    for n in range(1, nmax + 1):
        pred = n * f0 * math.sqrt(1 + B * n * n)
        if pred > min(max_hz, 0.45 * sr):
            break
        p = peak_near(S, df, pred, min(0.25 * f0, 0.03 * pred + 2 * df))
        if p is None:
            continue
        f, a = p
        if a > 30 * floor:
            found.append((n, f, a))
        if len(found) >= 4 and n % 3 == 0:
            ns = np.array([q[0] for q in found], float)
            fs = np.array([q[1] for q in found], float)
            A = np.vstack([np.ones_like(ns), ns ** 2]).T
            coef, *_ = np.linalg.lstsq(A, (fs / ns) ** 2, rcond=None)
            if coef[0] > 0:
                f0 = math.sqrt(coef[0])
                B = max(0.0, coef[1] / coef[0])
    return f0, B, found


def envelope(x, sr, f, f0, t0, hop=0.005):
    t = np.arange(len(x) - t0) / sr
    z = x[t0:] * np.exp(-2j * np.pi * f * t)
    w = max(int(3.0 / f0 * sr), int(0.02 * sr))
    k = np.hanning(w)
    k /= k.sum()
    h = int(hop * sr)
    env = np.abs(np.convolve(z, k, mode='same'))[::h]
    return np.arange(len(env)) * hop, 20 * np.log10(env + 1e-12)


def t60(ts, db, a, b, floor):
    m = (ts >= a) & (ts <= b) & (db > floor + 8)
    if m.sum() < 10:
        return None
    slope = np.polyfit(ts[m], db[m], 1)[0]
    return -60.0 / slope if slope < -1e-3 else float('inf')


def beating(ts, db, a, b, floor):
    m = (ts >= a) & (ts <= b) & (db > floor + 10)
    if m.sum() < 30:
        return None
    fit = np.polyval(np.polyfit(ts[m], db[m], 3), ts[m])
    r = db[m] - fit
    return float(np.sqrt(np.mean(r ** 2)))


def centroid(seg, sr):
    S, df = spectrum(seg, sr, pad=1)
    f = np.arange(len(S)) * df
    return float((S ** 2 * f).sum() / ((S ** 2).sum() + 1e-30))


def note(path, midi, start=0.0, length=None):
    x, sr = load(path, start, length)
    t0 = onset(x, sr)
    dur = (len(x) - t0) / sr
    f_guess = 440 * 2 ** ((midi - 69) / 12)
    f0, B, found = partials(x, sr, f_guess, t0, length=min(1.0, max(0.3, dur - 0.1)))
    out = {'file': path.split('/')[-1], 'midi': midi, 'sr': sr, 'dur': round(dur, 2),
           'f0': round(f0, 3), 'cents': round(1200 * math.log2(f0 / f_guess), 2),
           'B': B, 'partials': len(found)}
    rows = []
    for n, f, a in found[:16]:
        ts, db = envelope(x, sr, f, f0, t0)
        floor = np.percentile(db, 5)
        rows.append({'n': n, 'hz': round(f, 1),
                     'level': round(float(db[(ts > 0.02) & (ts < 0.08)].max()) if len(db) > 20 else -200, 1),
                     'early': t60(ts, db, 0.04, 0.4, floor),
                     'late': t60(ts, db, 0.8, min(3.5, dur - 0.1), floor),
                     'beat_db': beating(ts, db, 0.1, min(3.0, dur - 0.1), floor)})
    out['per_partial'] = rows
    # Pitch glide: the strongest of the first four partials' frequency in
    # short windows, in cents relative to its frequency 2 to 2.5 s in.
    if rows:
        best = max((r for r in rows if r['n'] <= 4), key=lambda r: r['level'], default=None)
        if best is not None and dur > 2.6:
            def freq(at):
                w = int(max(0.08, 8.0 / best['hz']) * sr)
                hop = int(0.01 * sr)
                i = t0 + int(at * sr)
                seg1, seg2 = x[i:i + w], x[i + hop:i + hop + w]
                t = np.arange(w) / sr
                ph = lambda s: np.angle(np.sum(s * np.hanning(w) * np.exp(-2j * np.pi * best['hz'] * t)))
                d = (ph(seg2) - ph(seg1) + np.pi) % (2 * np.pi) - np.pi
                return best['hz'] + d / (2 * np.pi * hop / sr)
            ref = np.median([freq(2.0 + 0.05 * k) for k in range(10)])
            out['glide_cents'] = [round(1200 * math.log2(freq(a) / ref), 1) for a in (0.05, 0.15, 0.4, 1.0)]
    a = x[t0:t0 + int(0.02 * sr)]
    b = x[t0 + int(0.3 * sr):t0 + int(0.5 * sr)]
    out['centroid_attack'] = round(centroid(a, sr)) if len(a) > 64 else None
    out['centroid_300ms'] = round(centroid(b, sr)) if len(b) > 64 else None
    # Inharmonic (noise) share of the first 30 ms: energy away from partials.
    seg = x[t0:t0 + int(0.03 * sr)]
    S, df = spectrum(seg, sr, pad=8)
    fr = np.arange(len(S)) * df
    near = np.zeros_like(S, bool)
    for n in range(1, 200):
        fn = n * f0 * math.sqrt(1 + B * n * n)
        if fn > 0.45 * sr:
            break
        near |= np.abs(fr - fn) < 0.2 * f0
    band = fr < 12000
    out['attack_noise'] = round(float((S[band & ~near] ** 2).sum() / ((S[band] ** 2).sum() + 1e-30)), 3)
    # Overall decay of the whole sound, RMS in 50 ms frames.
    h = int(0.05 * sr)
    frames = x[t0:t0 + (len(x) - t0) // h * h].reshape(-1, h)
    rms = 20 * np.log10(np.sqrt((frames ** 2).mean(1)) + 1e-12)
    out['rms_db_per_s'] = [round(float(v), 1) for v in rms[::4][:12]]
    return out


def ltas(path, start=0.0, length=None):
    x, sr = load(path, start, length)
    t0 = onset(x, sr)
    seg = x[t0:t0 + int(min(2.0, (len(x) - t0) / sr) * sr)]
    S, df = spectrum(seg, sr, pad=1)
    fr = np.arange(len(S)) * df
    bands = []
    f = 60.0
    while f < 12000:
        lo, hi = f * 2 ** (-1 / 12), f * 2 ** (1 / 12)
        m = (fr >= lo) & (fr < hi)
        bands.append((round(f), round(10 * math.log10((S[m] ** 2).mean() + 1e-30), 1) if m.any() else None))
        f *= 2 ** (1 / 6)
    return {'file': path.split('/')[-1], 'bands': bands}


def main():
    a = sys.argv[1:]
    def opt(name, default=None):
        if name in a:
            i = a.index(name)
            return float(a[i + 1])
        return default
    if a[0] == 'note':
        print(json.dumps(note(a[1], int(a[2]), opt('--start', 0.0), opt('--len'))))
    elif a[0] == 'ltas':
        print(json.dumps(ltas(a[1], opt('--start', 0.0), opt('--len'))))


if __name__ == '__main__':
    main()
