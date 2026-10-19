"""Onset-by-onset decay and brightness: mute.py FILE... -> per file: notes,
median dB fall from 30 ms to 230 ms after each onset's peak, median
spectral centroid 20-120 ms after it."""
import sys, os, statistics, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gtr
for path in sys.argv[1:]:
    x, sr = gtr.load(path)
    h = int(0.005 * sr)
    frames = x[:len(x) // h * h].reshape(-1, h)
    env = 20 * np.log10(np.sqrt((frames ** 2).mean(1)) + 1e-9)
    onsets = []
    for i in range(4, len(env) - 50):
        if env[i] - env[i - 4] > 12 and env[i] > env.max() - 40 and (not onsets or i - onsets[-1] > 40):
            onsets.append(i)
    falls, cents = [], []
    for i in onsets:
        p = i + int(np.argmax(env[i:i + 10]))
        a, b = p + 6, p + 46
        if b < len(env):
            falls.append(env[a] - env[b])
            seg = x[(p + 4) * h:(p + 24) * h]
            cents.append(gtr.centroid(seg, sr))
    if falls:
        print(f"{os.path.basename(path)[:60]:60} notes {len(falls):3d}  fall 30->230 ms {statistics.median(falls):5.1f} dB  centroid {statistics.median(cents):6.0f} Hz")
