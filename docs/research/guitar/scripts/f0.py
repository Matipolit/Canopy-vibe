#!/usr/bin/env python3
"""Quick audio facts: onset, f0 (nearest note), peak, ring time.
Usage: f0.py [--ch N] file [file ...]
Decodes with ffmpeg to float32 mono (or channel N) at native rate.
"""
import sys, subprocess, json
import numpy as np

NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]


def probe(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries",
                          "stream=sample_rate,channels,codec_name,bits_per_sample,bits_per_raw_sample,bit_rate,sample_fmt",
                          "-show_entries", "format=duration,bit_rate", "-of", "json", path], capture_output=True, text=True).stdout
    return json.loads(out)


def load(path, ch=None):
    info = probe(path)
    st = info["streams"][0]
    sr = int(st["sample_rate"])
    nch = int(st["channels"])
    af = ["-ac", "1"] if ch is None else ["-af", f"pan=mono|c0=c{ch}"]
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", path] + af + ["-f", "f32le", "-acodec", "pcm_f32le", "-"],
                         capture_output=True).stdout
    return np.frombuffer(raw, dtype=np.float32).astype(np.float64), sr, info


def midi_name(m):
    m = int(round(m))
    return f"{NAMES[m % 12]}{m // 12 - 1}"


def onset(x, sr):
    hop = int(sr * 0.002)
    n = len(x) // hop
    e = np.sqrt(np.mean(x[: n * hop].reshape(n, hop) ** 2, axis=1) + 1e-20)
    db = 20 * np.log10(e)
    pk = db.max()
    floor = np.percentile(db[: max(5, n // 20)], 50) if n > 20 else db.min()
    thr = max(floor + 12, pk - 40)
    idx = np.argmax(db > thr)
    return idx * hop / sr, db, hop


def f0_est(seg, sr, fmin=60, fmax=1400):
    seg = seg - seg.mean()
    w = np.hanning(len(seg))
    N = 1 << int(np.ceil(np.log2(len(seg) * 4)))
    X = np.abs(np.fft.rfft(seg * w, N))
    # autocorrelation via power spectrum
    ac = np.fft.irfft(X ** 2)[: len(seg)]
    ac /= ac[0] + 1e-20
    lo, hi = int(sr / fmax), int(sr / fmin)
    lag = lo + np.argmax(ac[lo:hi])
    # prefer shortest lag with nearly max peak (avoid octave-down)
    best = ac[lag]
    for L in range(lo, hi):
        if ac[L] > 0.9 * best and ac[L] >= ac[L - 1] and ac[L] >= ac[L + 1]:
            lag = L
            break
    # parabolic interp
    a, b, c = ac[lag - 1], ac[lag], ac[lag + 1]
    p = 0.5 * (a - c) / (a - 2 * b + c + 1e-20)
    f = sr / (lag + p)
    # refine with spectral peak near f
    freqs = np.fft.rfftfreq(N, 1 / sr)
    band = (freqs > f * 0.94) & (freqs < f * 1.06)
    if band.any():
        k = np.argmax(np.where(band, X, 0))
        if 0 < k < len(X) - 1:
            a, b, c = np.log(X[k - 1] + 1e-20), np.log(X[k] + 1e-20), np.log(X[k + 1] + 1e-20)
            p = 0.5 * (a - c) / (a - 2 * b + c + 1e-20)
            f = (k + p) * sr / N
    return f, ac[lag]


def main():
    args = sys.argv[1:]
    ch = None
    if args[0] == "--ch":
        ch = int(args[1]); args = args[2:]
    for path in args:
        try:
            x, sr, info = load(path, ch)
        except Exception as e:
            print(f"{path.split('/')[-1]}\tSKIP ({e.__class__.__name__})")
            continue
        st = info["streams"][0]
        t0, db, hop = onset(x, sr)
        i0 = int((t0 + 0.08) * sr)
        seg = x[i0: i0 + int(0.5 * sr)]
        if len(seg) < sr * 0.1:
            seg = x[i0:]
        f, conf = f0_est(seg, sr) if len(seg) > 1000 else (0, 0)
        m = 69 + 12 * np.log2(f / 440) if f > 0 else 0
        pk = db.max()
        ipk = int(np.argmax(db))
        below = np.where(db[ipk:] < pk - 40)[0]
        ring40 = below[0] * hop / sr if len(below) else (len(db) - ipk) * hop / sr
        peak = 20 * np.log10(np.max(np.abs(x)) + 1e-20)
        print(f"{path.split('/')[-1]}\tsr={sr}\tch={st['channels']}\tfmt={st.get('codec_name')}/{st.get('sample_fmt')}\tdur={len(x)/sr:.2f}\tonset={t0:.3f}\tf0={f:.1f}Hz\tnote={midi_name(m) if f>0 else '?'}({m:+.2f})\tconf={conf:.2f}\tpeak={peak:.1f}dBFS\tt(-40dB)={ring40:.2f}s")


if __name__ == "__main__":
    main()
