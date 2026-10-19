#!/usr/bin/env python3
"""Spectral summary of a segment: centroid, fraction of energy >2k/>5k, crest factor, noise floor, per-second RMS. Usage: spec.py file [t0 t1]"""
import sys, subprocess, numpy as np
p = sys.argv[1]
raw = subprocess.run(["ffmpeg","-v","error","-i",p,"-ac","1","-f","f32le","-"],capture_output=True).stdout
sr = int(subprocess.run(["ffprobe","-v","error","-show_entries","stream=sample_rate","-of","csv=p=0",p],capture_output=True,text=True).stdout.split()[0])
x = np.frombuffer(raw,np.float32).astype(float)
t0 = float(sys.argv[2]) if len(sys.argv)>2 else 0.2; t1 = float(sys.argv[3]) if len(sys.argv)>3 else t0+1.0
s = x[int(t0*sr):int(t1*sr)]
X = np.abs(np.fft.rfft(s*np.hanning(len(s))))**2; f = np.fft.rfftfreq(len(s),1/sr)
cen = (X*f).sum()/X.sum()
rms = [20*np.log10(np.sqrt(np.mean(x[i*sr:(i+1)*sr]**2))+1e-12) for i in range(int(len(x)/sr))]
print(f"{p.split('/')[-1]}: centroid={cen:.0f}Hz  E>2k={X[f>2000].sum()/X.sum():.4f}  E>5k={X[f>5000].sum()/X.sum():.5f}  crest={20*np.log10(np.abs(s).max()/np.sqrt(np.mean(s**2))):.1f}dB  rms/s=" + " ".join(f"{r:.0f}" for r in rms[:20]))
