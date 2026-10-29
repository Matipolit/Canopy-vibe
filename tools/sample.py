"""A sampling profiler made of gdb, for machines where perf isn't allowed
(`kernel.perf_event_paranoid` above 2, as on the development laptop).

gdb runs the program; a thread sends it SIGPROF every few milliseconds, gdb
stops on each one, records every busy thread's backtrace, and continues.
At the end it prints the hottest functions by self and inclusive time, and
with SAMPLE_OUT writes the folded stacks (one stack per line, `a;b;c count`)
for a flamegraph or for `tools/sample_summary.py`.

    gdb -batch -x tools/sample.py --args <program> [args]

    SAMPLE_MS      milliseconds between samples (default 5)
    SAMPLE_OUT     where to write the folded stacks
    SAMPLE_THREAD  sample only threads whose name contains this (a test's
                   thread is named after the test); a stop that walks every
                   thread's stack is slow in a program with many threads
    SAMPLE_PCS     where to write how often each instruction address was
                   the one running (`address count` per line), to line up
                   with `objdump -d` for a hot loop

Build what's profiled with line tables so frames have names:
`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release ...`.
Each stop costs about a millisecond per thread, so the program runs several
times slower than usual: time it separately. For instance:

    CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \\
        cargo build --release -p engine --example bench
    SAMPLE_OUT=/tmp/render.folded gdb -batch -x tools/sample.py \\
        --args target/release/examples/bench 10
    python3 tools/sample_summary.py /tmp/render.folded 40 Engine::render
"""

import collections
import os
import signal
import threading
import time

import gdb

gdb.execute("set pagination off")
gdb.execute("set confirm off")
gdb.execute("set startup-with-shell off")
gdb.execute("set print thread-events off")
# `noprint` would imply `nostop`, so the signal is printed and the output
# filtered by hand.
gdb.execute("handle SIGPROF stop print nopass")

interval = float(os.environ.get("SAMPLE_MS", "5")) / 1000.0
out = os.environ.get("SAMPLE_OUT")
pcs_out = os.environ.get("SAMPLE_PCS")
pcs = collections.Counter()
inclusive = collections.Counter()
selfc = collections.Counter()
stacks = collections.Counter()
total = 0
stop = False


def ticker():
    # A signal landing between gdb's fork and the program's exec kills the
    # child, so the first one waits until the program is surely running.
    time.sleep(1.0)
    while not stop:
        time.sleep(interval)
        # gdb's API is for its own thread only, so the inferior is found
        # through /proc: it's gdb's child.
        try:
            me = os.getpid()
            with open(f"/proc/{me}/task/{me}/children") as fh:
                for pid in fh.read().split():
                    os.kill(int(pid), signal.SIGPROF)
        except Exception:
            pass


threading.Thread(target=ticker, daemon=True).start()


def clean(name):
    if name is None:
        return "?"
    # Without the hash rustc appends to every symbol.
    parts = name.split("::")
    if parts and parts[-1].startswith("h") and len(parts[-1]) == 17:
        parts = parts[:-1]
    return "::".join(parts)


# Where a thread sits while it waits, which isn't work.
IDLE = {
    "syscall",
    "__syscall_cancel_arch",
    "__GI___futex_abstimed_wait_cancelable64",
    "__futex_abstimed_wait_common",
    "__futex_abstimed_wait_common64",
    "clock_nanosleep",
    "__GI___clock_nanosleep",
    "__GI___poll",
    "epoll_wait",
    "__GI___libc_read",
}


# Only threads whose name contains this (a Rust test's thread is named after
# the test) when set: walking every thread's stack is what makes a stop slow.
thread_filter = os.environ.get("SAMPLE_THREAD")


def record():
    """One sample of every thread that's doing something."""
    global total
    for thread in gdb.selected_inferior().threads():
        if thread_filter and thread_filter not in (thread.name or ""):
            continue
        try:
            thread.switch()
            frames = []
            f = gdb.newest_frame()
            pc = f.pc()
            while f is not None:
                frames.append(clean(f.name()))
                f = f.older()
        except gdb.error:
            continue
        if not frames or frames[0] in IDLE:
            continue
        total += 1
        pcs[pc] += 1
        selfc[frames[0]] += 1
        for name in set(frames):
            inclusive[name] += 1
        stacks[";".join(reversed(frames))] += 1


try:
    gdb.execute("run")
    while gdb.selected_inferior().pid != 0:
        record()
        gdb.execute("continue")
except gdb.error as e:
    print("gdb:", e, "after", total, "samples")
stop = True

print(f"\n{total} samples\n")
print("self:")
for name, n in selfc.most_common(45):
    print(f"{100.0 * n / total:6.1f}%  {name}")
print("\ninclusive:")
for name, n in inclusive.most_common(45):
    print(f"{100.0 * n / total:6.1f}%  {name}")
if pcs_out:
    with open(pcs_out, "w") as fh:
        for pc, n in pcs.most_common():
            fh.write(f"{pc:x} {n}\n")
if out:
    with open(out, "w") as fh:
        for stack, n in stacks.items():
            fh.write(f"{stack} {n}\n")
