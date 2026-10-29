/*
 * A sampling profiler to preload into a program, for machines where perf
 * isn't allowed (`kernel.perf_event_paranoid` above 2). Unlike
 * tools/sample.py, which stops the program from gdb, it never leaves the
 * process: a CPU-time timer (setitimer, ITIMER_PROF) interrupts whichever
 * thread is running, and the signal handler notes the instruction it was
 * at. That costs well under a microsecond per sample, so the program runs
 * at its usual speed and the profile shows where its time really goes.
 * Only the newest frame is recorded (no stacks): what it answers is which
 * instructions, and so which inlined loops, the time is spent in.
 *
 *     cc -O2 -shared -fPIC -o target/pcprof.so tools/pcprof.c
 *     PCPROF_OUT=target/pcs.txt LD_PRELOAD=target/pcprof.so <program> [args]
 *     python3 tools/pcprof_report.py target/pcs.txt <program> [top]
 *
 *     PCPROF_OUT  where to write the samples (default pcprof.txt)
 *     PCPROF_US   microseconds of CPU time between samples (default 500)
 *
 * The output is the executable's load address and path, then one sampled
 * address per line, as hexadecimal.
 */
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <ucontext.h>

#define CAPACITY (1 << 22)

static uintptr_t samples[CAPACITY];
static volatile unsigned long count;

static void on_tick(int sig, siginfo_t *info, void *context) {
    (void)sig;
    (void)info;
    ucontext_t *uc = context;
    unsigned long i = __atomic_fetch_add(&count, 1, __ATOMIC_RELAXED);
    if (i < CAPACITY) {
        samples[i] = (uintptr_t)uc->uc_mcontext.gregs[REG_RIP];
    }
}

__attribute__((constructor)) static void start(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = on_tick;
    action.sa_flags = SA_SIGINFO | SA_RESTART;
    sigaction(SIGPROF, &action, NULL);
    const char *us = getenv("PCPROF_US");
    long interval = us ? atol(us) : 500;
    struct itimerval timer = {{0, interval}, {0, interval}};
    setitimer(ITIMER_PROF, &timer, NULL);
}

__attribute__((destructor)) static void finish(void) {
    struct itimerval off = {{0, 0}, {0, 0}};
    setitimer(ITIMER_PROF, &off, NULL);
    const char *path = getenv("PCPROF_OUT");
    FILE *out = fopen(path ? path : "pcprof.txt", "w");
    if (!out) {
        return;
    }
    /* The executable's first mapping: addresses are relative to it. */
    FILE *maps = fopen("/proc/self/maps", "r");
    char line[4096];
    if (maps && fgets(line, sizeof line, maps)) {
        unsigned long base = strtoul(line, NULL, 16);
        char *path_start = strchr(line, '/');
        fprintf(out, "%lx %s", base, path_start ? path_start : "?\n");
    }
    if (maps) {
        fclose(maps);
    }
    unsigned long n = count < CAPACITY ? count : CAPACITY;
    for (unsigned long i = 0; i < n; i++) {
        fprintf(out, "%lx\n", (unsigned long)samples[i]);
    }
    fclose(out);
}
