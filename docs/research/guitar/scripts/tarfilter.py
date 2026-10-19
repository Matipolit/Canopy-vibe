#!/usr/bin/env python3
"""Stream a .tar.gz from stdin; write regular-file members whose path matches REGEX into OUTDIR (basename only).
Usage: curl ... | tarfilter.py OUTDIR REGEX"""
import sys, os, re, tarfile, shutil
out, rx = sys.argv[1], re.compile(sys.argv[2])
os.makedirs(out, exist_ok=True)
n = k = 0
with tarfile.open(fileobj=sys.stdin.buffer, mode="r|gz") as tf:
    for m in tf:
        n += 1
        if not m.isfile() or not rx.search(m.name):
            continue
        name = os.path.basename(m.name)
        if not name or name.startswith("."):
            continue
        src = tf.extractfile(m)
        with open(os.path.join(out, name), "wb") as dst:
            shutil.copyfileobj(src, dst)
        k += 1
print(f"scanned {n} members, kept {k}", file=sys.stderr)
