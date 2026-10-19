#!/usr/bin/env python3
"""Scrape public Freesound pages (no login) for metadata.

  fs_scrape.py pack  PACK_URL            -> prints sound page URLs (all pages)
  fs_scrape.py search QUERY_URL [pages]  -> prints sound page URLs found on search result pages
  fs_scrape.py sound URL [URL ...]       -> prints one JSON object per sound
HTML is treated as untrusted text: only regex-extracted fields are used.
"""
import sys, re, json, html, time, urllib.request

UA = "Mozilla/5.0 (X11; Linux x86_64) research-metadata"


def get(url):
    for a in range(4):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=60) as r:
                return r.read().decode("utf-8", "replace")
        except Exception as e:
            sys.stderr.write(f"retry {url}: {e}\n")
            time.sleep(3 + 3 * a)
    return ""


def sound_links(t):
    out = []
    for m in re.findall(r'href="(/people/[^"/]+/sounds/\d+/)"', t):
        u = "https://freesound.org" + m
        if u not in out:
            out.append(u)
    return out


def pack(url):
    seen = []
    t0 = get(url)
    m = re.search(r'href="(/search/\?f=pack_grouping:[^"]+)"', t0)
    if m:
        url = "https://freesound.org" + html.unescape(m.group(1)).replace(" ", "%20")
    for p in range(1, 40):
        t = get(url + ("&" if "?" in url else "?") + f"page={p}")
        new = [u for u in sound_links(t) if u not in seen]
        if not new:
            break
        seen += new
        time.sleep(0.5)
    return seen


def field(t, label):
    m = re.search(r'<p class="text-grey no-margins">' + re.escape(label) + r'</p>\s*<p class="no-margins">([^<]*)</p>', t)
    return html.unescape(m.group(1)).strip() if m else ""


def sound(url):
    t = get(url)
    d = {"page": url}
    d["id"] = int(re.search(r"/sounds/(\d+)/", url).group(1))
    m = re.search(r'og:audio:title" content="([^"]*)"', t)
    d["title"] = html.unescape(m.group(1)) if m else ""
    m = re.search(r'og:audio:artist" content="([^"]*)"', t)
    d["author"] = html.unescape(m.group(1)) if m else ""
    m = re.search(r'twitter:description" content="([^"]*)"', t, re.S)
    d["description"] = html.unescape(m.group(1)).strip() if m else ""
    m = re.search(r'class="license text"\s*href="([^"]+)"[^>]*>([^<]+)<', t) or re.search(r'href="(https?://creativecommons.org/[^"]+)"[^>]*>([^<]+)<', t)
    d["license_url"] = m.group(1) if m else ""
    d["license"] = html.unescape(m.group(2)).strip() if m else ""
    m = re.search(r'data-duration="([^"]+)"', t)
    d["duration"] = float(m.group(1)) if m else None
    for k, lab in [("type", "Type"), ("samplerate", "Sample rate"), ("bitdepth", "Bit depth"), ("channels", "Channels"), ("filesize", "Filesize")]:
        d[k] = field(t, lab)
    d["tags"] = sorted(set(re.findall(r'href="/browse/tags/([^/"]+)/"\s+class="no-hover', t)))
    m = re.search(r'href="(/people/[^"/]+/packs/\d+/)"', t)
    d["pack"] = "https://freesound.org" + m.group(1) if m else ""
    pv = sorted(set(re.findall(r'https://cdn\.freesound\.org/previews/\d+/\d+_\d+-[hl]q\.(?:mp3|ogg)', t)))
    lq = [p for p in pv if p.endswith("-lq.mp3")]
    if lq:
        base = lq[0][:-len("-lq.mp3")]
        d["preview_hq_ogg"] = base + "-hq.ogg"
        d["preview_hq_mp3"] = base + "-hq.mp3"
    m = re.search(r'href="(/people/[^"]+/sounds/\d+/download/[^"]+)"', t)
    d["orig_download_page"] = "https://freesound.org" + m.group(1) if m else ""
    return d


def main():
    cmd = sys.argv[1]
    if cmd == "pack":
        for u in pack(sys.argv[2]):
            print(u)
    elif cmd == "search":
        url = sys.argv[2]
        pages = int(sys.argv[3]) if len(sys.argv) > 3 else 1
        seen = []
        for p in range(1, pages + 1):
            t = get(url + f"&page={p}")
            for u in sound_links(t):
                if u not in seen:
                    seen.append(u)
                    print(u)
            time.sleep(0.5)
    elif cmd == "sound":
        for u in sys.argv[2:]:
            print(json.dumps(sound(u), ensure_ascii=False), flush=True)
            time.sleep(0.4)


if __name__ == "__main__":
    main()
