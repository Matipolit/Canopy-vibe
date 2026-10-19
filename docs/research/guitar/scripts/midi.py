#!/usr/bin/env python3
"""Minimal Standard MIDI File parser -> note list (seconds).
Usage: midi.py file.mid   -> prints TSV: onset offset channel pitch velocity track
"""
import sys, struct


def read_vlq(b, i):
    v = 0
    while True:
        c = b[i]; i += 1
        v = (v << 7) | (c & 0x7F)
        if not c & 0x80:
            return v, i


def parse(path):
    b = open(path, "rb").read()
    assert b[:4] == b"MThd", "not a MIDI file"
    hl = struct.unpack(">I", b[4:8])[0]
    fmt, ntrk, div = struct.unpack(">HHH", b[8:14])
    i = 8 + hl
    tracks = []
    for t in range(ntrk):
        while b[i:i + 4] != b"MTrk":
            i += 1
        ln = struct.unpack(">I", b[i + 4:i + 8])[0]
        tracks.append(b[i + 8:i + 8 + ln])
        i += 8 + ln
    events = []  # (tick, kind, data, track)
    for ti, tr in enumerate(tracks):
        j = 0; tick = 0; run = None
        while j < len(tr):
            dt, j = read_vlq(tr, j); tick += dt
            st = tr[j]
            if st == 0xFF:
                typ = tr[j + 1]; ln, j = read_vlq(tr, j + 2)
                data = tr[j:j + ln]; j += ln
                if typ == 0x51:
                    events.append((tick, "tempo", int.from_bytes(data, "big"), ti))
                if typ == 0x2F:
                    break
                continue
            if st in (0xF0, 0xF7):
                ln, j = read_vlq(tr, j + 1); j += ln
                continue
            if st & 0x80:
                run = st; j += 1
            s = run
            hi = s & 0xF0; ch = s & 0x0F
            n = 1 if hi in (0xC0, 0xD0) else 2
            d = tr[j:j + n]; j += n
            if hi == 0x90 and d[1] > 0:
                events.append((tick, "on", (ch, d[0], d[1]), ti))
            elif hi == 0x80 or (hi == 0x90 and d[1] == 0):
                events.append((tick, "off", (ch, d[0]), ti))
    events.sort(key=lambda e: (e[0], 0 if e[1] == "tempo" else 1))
    # tick -> seconds
    tempo = 500000; last_tick = 0; sec = 0.0
    if div & 0x8000:
        raise SystemExit("SMPTE division not supported")
    notes = []; active = {}
    for tick, kind, data, ti in events:
        sec += (tick - last_tick) * tempo / 1e6 / div; last_tick = tick
        if kind == "tempo":
            tempo = data
        elif kind == "on":
            ch, p, v = data
            active.setdefault((ch, p), []).append((sec, v, ti))
        else:
            ch, p = data
            if active.get((ch, p)):
                on, v, t = active[(ch, p)].pop(0)
                notes.append((on, sec, ch, p, v, t))
    notes.sort()
    return notes


if __name__ == "__main__":
    for n in parse(sys.argv[1]):
        print("%.4f\t%.4f\t%d\t%d\t%d\t%d" % n)
