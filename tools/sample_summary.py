"""Summarizes folded stacks from tools/sample.py: the hottest functions by
self time and by inclusive time, optionally only in stacks that pass
through a given function.

    python3 tools/sample_summary.py <folded> [top] [only]

`only` is a substring of a frame name, such as `Engine::render` to look
at the audio thread alone, or `CanopyApp as eframe` for the UI's frames.
"""

import collections
import sys

path = sys.argv[1]
top = int(sys.argv[2]) if len(sys.argv) > 2 else 40
only = sys.argv[3] if len(sys.argv) > 3 else None

selfc = collections.Counter()
inclusive = collections.Counter()
total = 0
for line in open(path):
    stack, count = line.rsplit(" ", 1)
    count = int(count)
    frames = stack.split(";")
    if only and not any(only in f for f in frames):
        continue
    total += count
    selfc[frames[-1]] += count
    for name in set(frames):
        inclusive[name] += count

print(f"{total} samples" + (f" through {only}" if only else ""))
print("\nself:")
for name, n in selfc.most_common(top):
    print(f"{100.0 * n / total:6.1f}%  {name}")
print("\ninclusive:")
for name, n in inclusive.most_common(top):
    print(f"{100.0 * n / total:6.1f}%  {name}")
