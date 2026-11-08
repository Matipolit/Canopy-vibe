#!/usr/bin/env python3
"""Compares canopy's render of an imported FL project with FL's own stems.

    tools/flp_compare.py <FL stems dir> <canopy stems dir> [--plots <dir>] [--only 3,14]
        [--fl-delay 5202] [--renumber 24:+1]

FL's "split playlist tracks" export writes `<song> - Track <n>.wav` per
playlist track (the whole mix, with only that track's clips playing); the
`flp_stems` UI test writes `Track <n>.wav` the same way. For each
pair this prints how far canopy's stem is shifted in time against FL's (from
the cross-correlation of their loudness envelopes), the level difference
while both play, where one plays and the other doesn't, and the difference in
each octave band. `--plots` also draws envelopes and spectra per track.

`--fl-delay` is how many samples late FL's stems start (FL's export of
right-here_2 runs 5202 samples behind its own timeline, on every stem).
`--renumber 24:+1` says canopy's track 24 and later are FL's 25 and later,
for stems exported after a playlist track was added.

Needs numpy; matplotlib for `--plots`.
"""

import argparse
import glob
import os
import re
import struct
import sys

import numpy as np

HOP = 0.010  # seconds per envelope frame
SILENT_DB = -60.0


def read_wav(path):
    """Returns (rate, float32 array [frames, channels])."""
    with open(path, "rb") as f:
        data = f.read()
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError(f"{path}: not a WAV file")
    pos = 12
    fmt = None
    while pos + 8 <= len(data):
        cid = data[pos : pos + 4]
        size = struct.unpack("<I", data[pos + 4 : pos + 8])[0]
        body = data[pos + 8 : pos + 8 + size]
        if cid == b"fmt ":
            tag, channels, rate, _, _, bits = struct.unpack("<HHIIHH", body[:16])
            if tag == 0xFFFE:
                tag = struct.unpack("<H", body[24:26])[0]
            fmt = (tag, channels, rate, bits)
        elif cid == b"data":
            tag, channels, rate, bits = fmt
            if tag == 3 and bits == 32:
                samples = np.frombuffer(body, dtype="<f4")
            elif tag == 1 and bits == 16:
                samples = np.frombuffer(body, dtype="<i2").astype(np.float32) / 32768.0
            elif tag == 1 and bits == 24:
                raw = np.frombuffer(body[: len(body) // 3 * 3], dtype=np.uint8).reshape(-1, 3)
                ints = (raw[:, 0].astype(np.int32) | (raw[:, 1].astype(np.int32) << 8)
                        | (raw[:, 2].astype(np.int32) << 16))
                ints = np.where(ints >= 1 << 23, ints - (1 << 24), ints)
                samples = ints.astype(np.float32) / float(1 << 23)
            elif tag == 1 and bits == 32:
                samples = np.frombuffer(body, dtype="<i4").astype(np.float32) / 2147483648.0
            else:
                raise ValueError(f"{path}: unsupported format {fmt}")
            frames = len(samples) // channels
            return rate, samples[: frames * channels].reshape(frames, channels)
        pos += 8 + size + (size & 1)
    raise ValueError(f"{path}: no data")


def envelope(audio, rate):
    """RMS in dB per HOP, of the channels' sum."""
    mono = audio.mean(axis=1) if audio.ndim == 2 else audio
    hop = int(rate * HOP)
    frames = len(mono) // hop
    if frames == 0:
        return np.zeros(0)
    blocks = mono[: frames * hop].reshape(frames, hop)
    rms = np.sqrt((blocks.astype(np.float64) ** 2).mean(axis=1))
    return 20 * np.log10(np.maximum(rms, 1e-9))


def best_shift(a, b, max_shift):
    """The shift in frames of `b` against `a` that best lines their
    envelopes up (positive: b is late)."""
    la = np.maximum(a, SILENT_DB) - SILENT_DB
    lb = np.maximum(b, SILENT_DB) - SILENT_DB
    n = max(len(la), len(lb))
    la = np.pad(la, (0, n - len(la)))
    lb = np.pad(lb, (0, n - len(lb)))
    best, best_score = 0, -1.0
    for shift in range(-max_shift, max_shift + 1):
        if shift >= 0:
            score = float(np.dot(la[: n - shift], lb[shift:]))
        else:
            score = float(np.dot(la[-shift:], lb[: n + shift]))
        if score > best_score:
            best, best_score = shift, score
    return best


def bands(audio, rate, start, end):
    """Energy in dB per octave band (31 Hz to 16 kHz) over [start, end) s."""
    mono = audio.mean(axis=1)
    piece = mono[int(start * rate) : int(end * rate)]
    if len(piece) < 4096:
        return None
    n = 4096
    window = np.hanning(n)
    spectra = []
    for i in range(0, len(piece) - n, n // 2):
        spectra.append(np.abs(np.fft.rfft(piece[i : i + n] * window)) ** 2)
    power = np.mean(spectra, axis=0)
    freqs = np.fft.rfftfreq(n, 1 / rate)
    centers = [31.25 * 2**k for k in range(10)]
    out = []
    for c in centers:
        mask = (freqs >= c / np.sqrt(2)) & (freqs < c * np.sqrt(2))
        out.append(10 * np.log10(max(power[mask].sum(), 1e-20)))
    return centers, np.array(out)


def spans(active):
    """Runs of True in a boolean array, as (start, end) frame pairs."""
    out = []
    start = None
    for i, a in enumerate(active):
        if a and start is None:
            start = i
        elif not a and start is not None:
            out.append((start, i))
            start = None
    if start is not None:
        out.append((start, len(active)))
    return out


def describe_spans(runs, limit=6):
    runs = [r for r in runs if (r[1] - r[0]) * HOP >= 0.25]
    text = ", ".join(f"{a * HOP:.1f}-{b * HOP:.1f}s" for a, b in runs[:limit])
    if len(runs) > limit:
        text += f", … ({len(runs)} in all)"
    return text or "none"


def compare(insert, fl_path, ours_path, plots, fl_delay=0):
    fl_rate, fl = read_wav(fl_path)
    fl = fl[fl_delay:]
    rate, ours = read_wav(ours_path)
    if fl_rate != rate:
        print(f"  rates differ: FL {fl_rate}, canopy {rate}")
    ef = envelope(fl, fl_rate)
    eo = envelope(ours, rate)
    shift = best_shift(ef, eo, int(0.5 / HOP))
    n = max(len(ef), len(eo))
    ef = np.pad(ef, (0, n - len(ef)), constant_values=-180)
    eo = np.pad(eo, (0, n - len(eo)), constant_values=-180)
    fl_on = ef > SILENT_DB
    our_on = eo > SILENT_DB
    both = fl_on & our_on
    peak_fl = 20 * np.log10(max(np.abs(fl).max(), 1e-9))
    peak_ours = 20 * np.log10(max(np.abs(ours).max(), 1e-9))
    print(f"Track {insert}: FL {len(fl) / fl_rate:.1f}s peak {peak_fl:.1f} dB, "
          f"canopy {len(ours) / rate:.1f}s peak {peak_ours:.1f} dB")
    if not fl_on.any():
        print("  FL's stem is silent")
        return
    if not our_on.any():
        print("  canopy's stem is silent")
        return
    print(f"  shift: canopy {shift * HOP * 1000:+.0f} ms against FL")
    if both.any():
        diff = eo[both] - ef[both]
        print(f"  level while both play: {np.median(diff):+.1f} dB median "
              f"(quartiles {np.percentile(diff, 25):+.1f} / {np.percentile(diff, 75):+.1f})")
    print(f"  FL only: {describe_spans(spans(fl_on & ~our_on))}")
    print(f"  canopy only: {describe_spans(spans(our_on & ~fl_on))}")
    # Loudest stretch of FL's stem, 4 s long, for the spectra.
    window = int(4 / HOP)
    if n > window:
        energy = np.convolve(np.maximum(ef, SILENT_DB) - SILENT_DB, np.ones(window), "valid")
        at = int(np.argmax(energy)) * HOP
    else:
        at = 0.0
    a = bands(fl, fl_rate, at, at + 4)
    b = bands(ours, rate, at, at + 4)
    if a and b:
        centers, da = a
        _, db = b
        rel = (db - db.max()) - (da - da.max())
        cells = " ".join(f"{c:>5.0f}:{d:+5.1f}" for c, d in zip(centers, rel))
        print(f"  spectrum at {at:.1f}s, canopy vs FL, each to its loudest band: {cells}")
    if plots:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        fig, (top, bottom) = plt.subplots(2, 1, figsize=(14, 7))
        t = np.arange(n) * HOP
        top.plot(t, np.maximum(ef, -90), label="FL", lw=0.6)
        top.plot(t, np.maximum(eo, -90), label="canopy", lw=0.6, alpha=0.8)
        top.set_ylim(-90, 6)
        top.set_title(f"Track {insert}: RMS (dB) per 10 ms")
        top.legend()
        if a and b:
            bottom.semilogx(centers, da, "o-", label=f"FL {at:.1f}-{at + 4:.1f}s")
            bottom.semilogx(centers, db, "o-", label="canopy")
            bottom.legend()
            bottom.set_title("octave bands (dB)")
        fig.tight_layout()
        fig.savefig(os.path.join(plots, f"track_{insert:02}.png"), dpi=80)
        plt.close(fig)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("fl")
    parser.add_argument("ours")
    parser.add_argument("--plots")
    parser.add_argument("--only")
    parser.add_argument("--fl-delay", type=int, default=0)
    parser.add_argument("--renumber")
    args = parser.parse_args()
    only = {int(x) for x in args.only.split(",")} if args.only else None
    if args.plots:
        os.makedirs(args.plots, exist_ok=True)
    fl_stems = {}
    for path in glob.glob(os.path.join(args.fl, "*.wav")):
        m = re.search(r"Track (\d+)\.wav$", path)
        if m:
            fl_stems[int(m.group(1))] = path
    ours = {}
    for path in glob.glob(os.path.join(args.ours, "Track *.wav")):
        m = re.search(r"Track (\d+)\.wav$", path)
        if m:
            n = int(m.group(1))
            if args.renumber:
                start, by = args.renumber.split(":")
                if n >= int(start):
                    n += int(by)
            ours[n] = path
    for insert in sorted(set(fl_stems) | set(ours)):
        if only and insert not in only:
            continue
        if insert not in ours:
            print(f"Track {insert}: FL has a stem, canopy none")
            continue
        if insert not in fl_stems:
            rate, audio = read_wav(ours[insert])
            loud = 20 * np.log10(max(np.abs(audio).max(), 1e-9))
            print(f"Track {insert}: canopy only (FL wrote no stem), peak {loud:.1f} dB")
            continue
        compare(insert, fl_stems[insert], ours[insert], args.plots, args.fl_delay)


if __name__ == "__main__":
    sys.exit(main())
