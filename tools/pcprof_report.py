"""Summarizes what tools/pcprof.c recorded: the functions the samples fell
in, by the innermost inlined function at each address (`self`) and by the
function it was compiled into (`symbol`), using addr2line for the inlining.
Addresses outside the executable (shared libraries) count by library.

    python3 tools/pcprof_report.py <pcs.txt> [top] [only]

`only` keeps the samples whose inline chain mentions it (a substring),
such as `render_node` for the audio thread's nodes; percentages are then
of those. Build with line tables (`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`)
so addresses have names.
"""

import collections
import re
import subprocess
import sys

path = sys.argv[1]
top = int(sys.argv[2]) if len(sys.argv) > 2 else 40
only = sys.argv[3] if len(sys.argv) > 3 else None

with open(path) as fh:
    header = fh.readline().split(None, 1)
    base = int(header[0], 16)
    exe = header[1].strip()
    pcs = [int(line, 16) for line in fh if line.strip()]

counts = collections.Counter(pcs)
inside = [pc for pc in counts if pc >= base]
offsets = [pc - base for pc in inside]


def clean(name):
    # Without generic arguments and the hash rustc appends; a trait
    # method keeps its type's name (`Reverb::process`).
    name = re.sub(r"::h[0-9a-f]{16}$", "", name)
    implementor = re.match(r"^<([^<>]*?)(?: as [^<>]*)?>::(.*)$", name)
    if implementor:
        name = implementor.group(1).split("::")[-1] + "::" + implementor.group(2)
    depth = 0
    out = []
    for ch in name:
        if ch == "<":
            depth += 1
        elif ch == ">":
            depth -= 1
        elif depth == 0:
            out.append(ch)
    return "".join(out).replace("::{closure#0}", "{closure}")


chains = {}
if offsets:
    query = "\n".join(f"{o:x}" for o in offsets) + "\n"
    # Each address's inline chain ends where the next address's output
    # starts; `-a` prints the address first to mark it.
    result = subprocess.run(
        ["addr2line", "-a", "-f", "-i", "-C", "-e", exe],
        input=query,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    current = None
    names = []
    for line in result:
        if line.startswith("0x"):
            if current is not None:
                chains[current] = names
            current = int(line, 16)
            names = []
        elif not line.startswith("/") and not line.startswith("??:") and ":" not in line[:2]:
            # Function names alternate with file:line.
            if not re.match(r"^.*:\d+( \(discriminator \d+\))?$", line):
                names.append(clean(line))
    if current is not None:
        chains[current] = names

selfc = collections.Counter()
symbol = collections.Counter()
inclusive = collections.Counter()
total = 0
for pc, n in counts.items():
    if pc >= base:
        chain = chains.get(pc - base, ["?"]) or ["?"]
    else:
        chain = ["(shared library)"]
    if only and not any(only in f for f in chain):
        continue
    total += n
    selfc[chain[0]] += n
    symbol[chain[-1]] += n
    for f in set(chain):
        inclusive[f] += n

print(f"{total} samples" + (f" mentioning {only}" if only else ""))
for title, counter in (("self", selfc), ("symbol", symbol), ("inclusive", inclusive)):
    print(f"\n{title}:")
    for name, n in counter.most_common(top):
        print(f"{100.0 * n / max(total, 1):6.1f}%  {name[:140]}")
