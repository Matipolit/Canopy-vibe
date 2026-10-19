#!/usr/bin/env python3
"""Read a remote zip via HTTP Range requests; list or extract selected members.

Usage:
  rangezip.py list   URL  [--out listing.tsv]
  rangezip.py get    URL  OUTDIR  member1 [member2 ...]
  rangezip.py getfile URL OUTDIR  members.txt   (one member name per line)

Extracted files are written flat into OUTDIR (path separators replaced by '__'),
so no member path can escape OUTDIR.
"""
import sys, os, io, zipfile, urllib.request, time

UA = "Mozilla/5.0 (research data fetch)"


class HTTPRangeFile(io.RawIOBase):
    def __init__(self, url, blocksize=1 << 20):
        self.url = url
        self.pos = 0
        self.blocksize = blocksize
        self.cache = {}  # block index -> bytes
        self.nreq = 0
        self.nbytes = 0
        req = urllib.request.Request(url, headers={"Range": "bytes=0-0", "User-Agent": UA})
        with urllib.request.urlopen(req) as r:
            cr = r.headers.get("Content-Range")
            if not cr:
                raise RuntimeError("server does not support ranges")
            self.size = int(cr.split("/")[-1])

    def readable(self):
        return True

    def seekable(self):
        return True

    def tell(self):
        return self.pos

    def seek(self, off, whence=0):
        if whence == 0:
            self.pos = off
        elif whence == 1:
            self.pos += off
        else:
            self.pos = self.size + off
        return self.pos

    def _fetch(self, start, end):  # inclusive end
        for attempt in range(6):
            try:
                req = urllib.request.Request(self.url, headers={"Range": f"bytes={start}-{end}", "User-Agent": UA})
                with urllib.request.urlopen(req, timeout=120) as r:
                    data = r.read()
                if len(data) != end - start + 1:
                    raise IOError(f"short read {len(data)} vs {end-start+1}")
                self.nreq += 1
                self.nbytes += len(data)
                return data
            except Exception as e:  # retry
                sys.stderr.write(f"retry {attempt} {start}-{end}: {e}\n")
                time.sleep(2 + 3 * attempt)
        raise IOError("failed range fetch")

    def read(self, n=-1):
        if n is None or n < 0:
            n = self.size - self.pos
        n = min(n, self.size - self.pos)
        if n <= 0:
            return b""
        start, end = self.pos, self.pos + n - 1
        # large reads: fetch directly
        if n > 4 * self.blocksize:
            data = self._fetch(start, end)
            self.pos += n
            return data
        b0, b1 = start // self.blocksize, end // self.blocksize
        missing = [b for b in range(b0, b1 + 1) if b not in self.cache]
        if missing:
            ms, me = missing[0], missing[-1]
            data = self._fetch(ms * self.blocksize, min(self.size, (me + 1) * self.blocksize) - 1)
            for b in range(ms, me + 1):
                o = (b - ms) * self.blocksize
                self.cache[b] = data[o:o + self.blocksize]
        buf = b"".join(self.cache[b] for b in range(b0, b1 + 1))
        if len(self.cache) > 64:
            self.cache = {}
        o = start - b0 * self.blocksize
        out = buf[o:o + n]
        self.pos += len(out)
        return out

    def readinto(self, b):
        d = self.read(len(b))
        b[:len(d)] = d
        return len(d)


def safe_name(member):
    return member.replace("\\", "/").strip("/").replace("/", "__").replace("..", "_")


def main():
    cmd, url = sys.argv[1], sys.argv[2]
    f = HTTPRangeFile(url)
    z = zipfile.ZipFile(f)
    if cmd == "list":
        out = sys.stdout
        if len(sys.argv) > 4 and sys.argv[3] == "--out":
            out = open(sys.argv[4], "w")
        for i in z.infolist():
            out.write(f"{i.filename}\t{i.file_size}\t{i.compress_size}\t{i.compress_type}\n")
        sys.stderr.write(f"{len(z.infolist())} members, {f.nreq} requests, {f.nbytes} bytes\n")
        return
    outdir = sys.argv[3]
    os.makedirs(outdir, exist_ok=True)
    if cmd == "get":
        members = sys.argv[4:]
    elif cmd == "getfile":
        members = [l.rstrip("\n") for l in open(sys.argv[4]) if l.strip()]
    else:
        raise SystemExit("bad cmd")
    names = set(z.namelist())
    for m in members:
        if m not in names:
            sys.stderr.write(f"MISSING {m}\n")
            continue
        dest = os.path.join(outdir, safe_name(m))
        if os.path.exists(dest) and os.path.getsize(dest) == z.getinfo(m).file_size:
            continue
        info = z.getinfo(m)
        # fetch this member's local header + data in one go to warm cache
        with z.open(info) as src, open(dest + ".part", "wb") as dst:
            while True:
                chunk = src.read(1 << 22)
                if not chunk:
                    break
                dst.write(chunk)
        os.replace(dest + ".part", dest)
        sys.stderr.write(f"got {m} -> {dest} ({info.file_size} B)\n")
    sys.stderr.write(f"{f.nreq} requests, {f.nbytes} bytes transferred\n")


if __name__ == "__main__":
    main()
