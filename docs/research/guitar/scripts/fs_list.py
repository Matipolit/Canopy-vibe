#!/usr/bin/env python3
import json,sys,re
rows=[json.loads(l) for f in sys.argv[1:] for l in open(f)]
seen=set()
for d in sorted(rows,key=lambda d:(d['author'].lower(),d['id'])):
    if d['id'] in seen: continue
    seen.add(d['id'])
    t=(d['title']+' '+d['description']+' '+' '.join(d['tags'])).lower()
    if not re.search(r'guitar',t): continue
    if not re.search(r'single|open|note|pluck|palm|mute|string|strum|harmonic|chord',t): continue
    if re.search(r'loop|bass|ukulele|uke|banjo|synth|riff|song|bpm|distort|drum|mandolin|delay|melod|progression|improv|jam|gain|fuzz|reverse|pitch pipe|bowed',t): continue
    if (d['duration'] or 0)>40: continue
    print(d['id'],d['author'][:14],'|',d['title'][:34],'|',d['license'].replace('Creative Commons 0','CC0').replace('Attribution','BY')[:18],'|',round(d['duration'] or 0,1),d['samplerate'].replace(' kHz',''),d['bitdepth'].replace(' bit',''),d['channels'][:2],'|',d['pack'].split('/')[-2] if d['pack'] else '-','|',d['description'][:80].replace('\n',' '))
