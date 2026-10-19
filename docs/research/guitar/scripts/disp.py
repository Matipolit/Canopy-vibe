import math
def ap_pd(a,w):
    return 1 - 2*math.atan2(a*math.sin(w), 1 + a*math.cos(w))/w
def design(b, hz, sr, stages=4, match=2500.0, maxpole=0.85, budget=0.25):
    n = round(match/hz)
    stretch = math.sqrt(1+b*n*n)
    pn = n*hz*stretch
    period = sr/hz
    w0 = 2*math.pi*hz/sr; wn = 2*math.pi*pn/sr
    wanted = period*(1-1/stretch)/stages
    spread = lambda a: ap_pd(a,w0)-ap_pd(a,wn)
    lo, hi = -maxpole, 0.0
    if spread(lo) <= wanted: a = lo
    else:
        for _ in range(40):
            mid=(lo+hi)/2
            if spread(mid) > wanted: lo = mid
            else: hi = mid
        a=(lo+hi)/2
    per = budget*period/stages
    return max(a, (1-per)/(1+per)), n
def partials(a, hz, sr, stages, N=40):
    # total loop delay at f0 equals period: line = P - stages*pd(f0); partial n: solve f*(line + stages*pd(f)) = n
    P = sr/hz; w0=2*math.pi*hz/sr
    line = P - stages*ap_pd(a,w0)
    out=[]
    for n in range(1,N+1):
        f = n*hz
        for _ in range(50):
            w=2*math.pi*f/sr
            d = line + stages*ap_pd(a,w)
            f = n*sr/d
        out.append(f)
    return out
sr=48000
for b,hz in [(1.6e-4,82.41),(1.0e-4,82.41),(9e-5,110),(1.4e-5,82.41),(8e-5,196)]:
    for match in [2500, 1200, 800]:
        a,n = design(b,hz,sr,match=match)
        ps = partials(a,hz,sr,4)
        # fitted B over partials <= 3 kHz
        import numpy as np
        ns=np.array([i+1 for i,f in enumerate(ps) if f<3000],float); fs=np.array([f for f in ps if f<3000])
        A=np.vstack([np.ones_like(ns),ns**2]).T; c,*_=np.linalg.lstsq(A,(fs/ns)**2,rcond=None)
        ideal=[ (i+1)*hz*math.sqrt(1+b*(i+1)**2) for i in range(len(ps))]
        err=[1200*math.log2(p/q) for p,q in zip(ps,ideal)]
        print(f"B={b:.1e} f0={hz} match={match}: a={a:.3f} n={n} fitB={c[1]/c[0]:.2e} cents err at n=5,10,20,30: {err[4]:+.1f} {err[9]:+.1f} {err[19]:+.1f} {err[29]:+.1f}")
