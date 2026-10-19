# Research report: pickups (A) and amp/cab modelling (B)

Delivered by the research helper the guitar agent started ("Research pickups and amp/cab modelling"). Condensed, with every figure and source kept.

Markers: [P] = primary source read; [S] = secondary source; [D] = the helper's own calculation from a primary formula; [UNSURE] = doubtful.

## A. Electric guitar pickups

### A1. Distances from the bridge saddle, and scale lengths

- **Scale lengths.** Fender 25.5 in (647.7 mm). Gibson 24.75 in nominal (628.65 mm), but often 24.56–24.62 in in practice [S].
- **Stratocaster, Mohamad/Dixon/Harte ICASSP 2017 [P]** (measured on a Squier, string length 652 mm): neck 160 mm, middle 102 mm, bridge 38.5–48 mm.
  - The bridge range is due to the slanted bridge pickup. The treble side is presumably at 38.5 mm (the paper doesn't say).
- **Stratocaster, Tillman 2002 [S]:** 162 / 98 / 41 mm. Paiva 2012 cites these.
- **Gibson-type humbuckers, Zollner PotEG §2.8.3 [P]:** bridge 45 mm, neck 147 mm.
  - Forum figures for a '59 Les Paul are similar: about 46 and 146 mm [S].
- **Humbucker pole-row spacing:** 18–19 mm. Mini-humbucker: 13 mm. Single-coil-size humbucker: 7.6 mm.
- **Position comb filter, Paiva, Pakarinen & Välimäki 2012 [P]:**
  - Point model: H_P(f, d) = sin(π d f / (L f0)).
  - Waveguide form: H = 1 − β·z^(−2·N3), with N3 = round(T3·fs), T3 = d/c and c = 2·L·f0. Use a fractional delay.
  - Two pickups in phase give H = 2[e^{jω(Ta+Tb)/2} − β e^{−jω(Ta+Tb)/2}]·cos(ω(Ta−Tb)/2). Out of phase, the cos becomes sin.
  - [UNSURE] Paiva's T3 = 0.29 ms for d = 4.1 cm looks wrong. d/(2·L·f0) gives about 0.386 ms.
- **Null check, Zollner:** the Strat bridge pickup's null on low E is at about 3.2 kHz without dispersion, and about 3.3–3.5 kHz with string stiffness.

### A2. Aperture (sensing width)

- **Zollner [P], measured:**
  - The magnetic window is only about 5–10 mm wide, roughly Gaussian, and independent of coil width.
  - The effect is minor for single coils, because the pickup resonance already limits the response to about 5 kHz.
  - Output falls about 3–4 dB per mm of string-to-magnet distance (around a 3 mm gap).
- **Humbuckers** sense the string at two points 18 mm apart:
  - The slug coil is 2–3 dB hotter than the screw coil.
  - The resulting comb has its first notch around 3 kHz on low E, and about 4 kHz on A [D].
- **Tillman's rectangular window [S]** (1 in for a single coil, 2.5 in for a humbucker) is far too wide.
- **Paiva [P]** suggests a Hamming-shaped FIR aperture in cascade with the comb.
- **[D] Rectangular model at Zollner's widths, low E:**
  - W = 10 mm: −3 dB at about 4.7 kHz.
  - W = 5 mm: −3 dB at about 9.5 kHz.
  - High E is 4× higher, so negligible.

### A3. Equivalent circuit and loaded resonance

- **Circuit:** EMF, then series R + L, then C (coil + cable + tone) in shunt, then R_l (pots ∥ amp input) in shunt.
- **Transfer function, Paiva eq. 30 [P]:** H_c(s) = 1 / [(1 + R/R_l) + s(L/R_l + R·C) + s²·L·C].
- **Bilinear transform, with c = 2fs:** H(z) = (1 + 2z⁻¹ + z⁻²)/(c0 + c1 z⁻¹ + c2 z⁻²), where
  - c0 = 1 + R/R_l + 2fs·L/R_l + 2fs·R·C + 4fs²·L·C
  - c1 = 2 + 2R/R_l − 8fs²·L·C
  - c2 = 1 + R/R_l − 2fs·L/R_l − 2fs·R·C + 4fs²·L·C

  The helper re-derived these and they check out.
- **Typical ranges:** L 1–10 H, R 5–15 kΩ, C 15–200 pF (plus cable), R_l 300 kΩ – 3 MΩ. Baseline example: L = 2 H, R = 10 kΩ, C = 50 pF, R_l = 1 MΩ.
- **Zollner [P]:**
  - 2–10 H with a 300–600 pF cable resonates at 2–5 kHz, with a peak up to about 10 dB.
  - Q with pots and amp connected is about 3; unloaded up to 9.
  - A Strat coil is about 2.2 H with magnets. Cable capacitance is about 100 pF/m. Amp input is 1 MΩ.
- **Lemme's measured table [S]** (L / winding C / resonance with 470 pF, 680 pF, 1 nF, 2.2 nF / max Q):
  - Fender Strat 1972: 2.2 H / 110 pF / 4.4, 3.8, 3.2, 2.2 kHz / Q 6.3.
  - Telecaster bridge: 2.9 H / 160 pF / 3.7, 3.2, 2.7, 1.9 / Q 3.7.
  - Gibson 490 humbucker: 3.8 H / 130 pF / 3.3, 2.9, 2.4, 1.7 / Q 2.0–2.5.
  - SD '59: 5.0 H / 120 pF / 2.9, 2.6, 2.2, 1.5 / Q 2.6.
  - P90: 6.6 H / 95 pF / 2.6, 2.2, 1.9, 1.3 / Q 2.9.
- **Lemme, other figures:**
  - Coils in parallel give L/4 (resonance ×2). A single coil gives L/2 (×√2).
  - Eddy currents lower the peak and steepen the roll-off to 18 dB/oct.
  - A load of 47 kΩ or less kills a Strat pickup's peak.
- **Strat wiring, Meiser [S]:**
  - About 6 kΩ and 2.5 H, 120 pF.
  - About 3.5 kHz with a 700 pF cable.
  - 250 kΩ pots (Gibson uses 500 kΩ); a 22 nF tone capacitor (50/100 nF in the 1950s).
  - Tone fully down puts the resonance near 600 Hz.
- **[D] Loaded responses:**
  - Strat (2.2 H / 6 kΩ / 110 + 500 pF / 111 kΩ load): about 4.0 kHz, +4.5 dB, Q about 1.6.
  - Humbucker (4.4 H / 8 kΩ / 130 + 500 pF / 200 kΩ): about 2.9 kHz, +6.2 dB, Q about 2.0.

  This model has no eddy losses, so it overstates covered humbuckers.

### A4. What the pickup senses, and its nonlinearity

- **Magnetic pickups** output EMF ∝ string velocity at one point (single coil) or two points (humbucker) [P Zollner ch. 6; S Lemme].
- **Piezo bridge pickups** respond to force, and form a high-pass with the load: about 106 Hz into 1 MΩ with 1.5 nF.
- **Nonlinearity, Paiva [P].** Model order: comb/aperture, then a static nonlinearity (displacement → flux), then d/dt, then the RLC filter.
  - Vertical motion follows a roughly exponential flux curve, adding odd and even harmonics up to about the 3rd.
  - Horizontal motion has a bell-shaped response, adding even harmonics (frequency doubling).
  - Lemme: the curve is "hyperbolic", with more even harmonics when the pickup is closer.
- **Further sources:**
  - Horton & Moore 2009, magnetic-charge model.
  - Novak et al., DAFx-16: Hammerstein identification (1.42 mm string, 5 mm gap, 1 mm amplitude).

## B. Amp and cab modelling

### B1. Pakarinen & Yeh 2009, CMJ 33(2):85–100 (paywalled; abstract only)

- A review: sources of nonlinearity, linear stages (tone stack, cabinet), nonlinear methods (waveshaping up to wave digital filters).
- Static waveshapers are typically oversampled up to 8× [S].

### B2. Fender Bassman tone stack (Yeh & Smith, DAFx-06; Yeh thesis 2009 §2.2) [P]

- **'59 Bassman values:** C1 = 250 pF, C2 = C3 = 20 nF, R1 = 250 kΩ (treble), R2 = 1 MΩ (bass), R3 = 25 kΩ (mid), R4 = 56 kΩ.
  - The thesis prints 45k for R4, probably a typo.
  - t and m are linear 0–1; the bass pot l is log taper.
- **Other stacks:**
  - JCM800: 470 pF, 22 nF, 22 nF, 220k, 1M, 22k (some sources say 25k), 33k.
  - Vox Top Boost: 50 pF, 22 nF, 22 nF, 1M, 1M, 10k, 100k.
  - Blackface Twin: 250 pF, 100 nF, 47 nF, 250k, 250k, 10k, 100k.
- **Transfer function:** H(s) = (b1 s + b2 s² + b3 s³) / (a0 + a1 s + a2 s² + a3 s³).
  - b1 = t·C1R1 + m·C3R3 + l(C1R2 + C2R2) + (C1R3 + C2R3)
  - b2 = t(C1C2R1R4 + C1C3R1R4) − m²(C1C3R3² + C2C3R3²) + m(C1C3R1R3 + C1C3R3² + C2C3R3²) + l(C1C2R1R2 + C1C2R2R4 + C1C3R2R4) + lm(C1C3R2R3 + C2C3R2R3) + (C1C2R1R3 + C1C2R3R4 + C1C3R3R4)
  - b3 = lm(C1C2C3R1R2R3 + C1C2C3R2R3R4) − m²(C1C2C3R1R3² + C1C2C3R3²R4) + m(C1C2C3R1R3² + C1C2C3R3²R4) + t·C1C2C3R1R3R4 − tm·C1C2C3R1R3R4 + tl·C1C2C3R1R2R4
  - a0 = 1
  - a1 = (C1R1 + C1R3 + C2R3 + C2R4 + C3R4) + m·C3R3 + l(C1R2 + C2R2)
  - a2 = m(C1C3R1R3 − C2C3R3R4 + C1C3R3² + C2C3R3²) + lm(C1C3R2R3 + C2C3R2R3) − m²(C1C3R3² + C2C3R3²) + l(C1C2R1R2 + C1C2R2R4 + C1C3R2R4 + C2C3R2R4) + (C1C2R1R4 + C1C3R1R4 + C1C2R3R4 + C1C2R1R3 + C1C3R3R4 + C2C3R3R4)
  - a3 = lm(C1C2C3R1R2R3 + C1C2C3R2R3R4) − m²(C1C2C3R1R3² + C1C2C3R3²R4) + m(C1C2C3R3²R4 + C1C2C3R1R3² − C1C2C3R1R3R4) + l·C1C2C3R1R2R4 + C1C2C3R1R3R4
- **Bilinear transform (c = 2/T):**
  - B0 = −b1c − b2c² − b3c³; B1 = −b1c + b2c² + 3b3c³; B2 = b1c + b2c² − 3b3c³; B3 = b1c − b2c² + b3c³
  - A0 = −a0 − a1c − a2c² − a3c³; A1 = −3a0 − a1c + a2c² + 3a3c³; A2 = −3a0 + a1c + a2c² − 3a3c³; A3 = −a0 + a1c − a2c² + a3c³

  Re-derived by the helper; they match.
- **Notes:**
  - The poles are real. Treble moves only the zeros.
  - At fs = 44.1 kHz the bilinear error reaches 3 dB around 13 kHz; oversample or prewarp.
- **[D] Responses:**
  - Bassman at noon: −12.4 dB dip at about 730 Hz.
  - JCM800 at noon: −9.2 dB at about 680 Hz.
  - Blackface Twin at noon: about −22.6 dB at about 400 Hz (low confidence).

### B3. Distortion pedals and the diode clipper (Yeh, Abel & Smith, DAFx-07) [P]

- **Diode clipper ODE:** dVo/dt = (Vi − Vo)/(RC) − (2·Is/C)·sinh(Vo/Vt).
  - Values: R = 2.2 kΩ, C = 10 nF, Is = 2.52 nA, Vt = 45.3 mV (includes the emission coefficient).
  - Implicit solvers (trapezoidal, BDF2, backward Euler) are fine at 8× oversampling. Forward Euler needs about 38×, RK4 about 30×.
  - Newton converges in about 1.03–1.38 iterations per sample.
  - A static approximation (prefilter plus the DC curve) has error under −40 dB.
- **DS-1:**
  - Gain stage H(s) = [(s + 1/(Rt·Cc))(s + 1/(Rb·Cz)) + s/(Rb·Cc)] / [(s + 1/(Rt·Cc))(s + 1/(Rb·Cz))], with Rt = D·100k, Rb = (1−D)·100k + 4.7k, Cz = 1 µF, Cc = 250 pF.
  - The BJT stage has 36 dB of gain with poles at 3 and 600 Hz.
  - The tone control blends a high-pass at 1.16 kHz and a low-pass at 320 Hz.
- **Tube Screamer:**
  - Clipper: dV/dt = In/Cc − V/(R2·Cc) − (2Is/Cc)·sinh(V/Vt), with Vo = V + Vi.
  - Cc = 51 pF, R2 = 51k + D·500k, R1 = 4.7k, Cz = 47 nF.
  - Tone stage: Rf = 1k, Rr = (1−T)·20k, Rl = T·20k, Rz = 220 Ω, Cz = 0.22 µF, Ri = 10k, Rs = 1k, Cs = 0.22 µF. Eq. 24 [UNSURE]; check it against the PDF.
  - Cheap nonlinearity: x/(1 + |x|^n)^(1/n); n = 2.5 approximates tanh.
- **The authors' caveat:** real pedals show even harmonics that symmetric clippers lack.

### B4. Speakers and cabinets

- **Zollner PotEG ch. 11 / DAGA 2012 [P]:**
  - Main resonance 70–100 Hz; a small sealed box can reach about 160 Hz.
  - Cabinet standing waves between 200 and 2000 Hz.
  - Open-back cabinets are louder above 160–200 Hz but weaker in the bass.
  - Cone breakup starts at about 350 Hz. The on-axis response stays decent up to about 5 kHz, then drops sharply, with ripple from about 1.5 kHz.
  - Voice-coil inductance is about 1 mH.
  - With a high output impedance (no negative feedback, e.g. 200 Ω), the speaker's impedance shape shows up in the response: up to +17 dB at the resonance. An AC30 combo peaks at about 70 Hz (speaker), about 180 Hz (cabinet Helmholtz) and in the treble (voice-coil inductance).
- **Yeh, Bank & Karjalainen, DAFx-08 [P]:**
  - Engl combo with a Celestion G12 V30: f0 = 71.2 Hz, Qts = 0.64.
  - A 16th-order parallel second-order IIR is perceptually adequate.
  - Distortion at 30 W, in dB below the fundamental (2nd / 3rd / 4th / 5th / THD):
    - 70 Hz: −11 / −13 / −28 / −37 / −9
    - 100 Hz: −27 / −29 / … / THD −25
    - 140 Hz: THD −36
    - 200–400 Hz: THD −39
    - 4 kHz: THD −37
- **Manufacturer data [S]:**
  - Celestion V30: Fs 75 Hz, 70–5000 Hz, 100 dB.
  - Celestion G12M Greenback: Fs 75 Hz, 75–5000 Hz, 98 dB.
  - Jensen P12R: Fs about 80 Hz, Qts about 2.1, Le about 0.5 mH.

### B5. Clean versus crunch, and tube harmonics

- **Tone-stack dips:** see B2.
- **Mačák & Schimmel, DAFx-11 [P]:** single-ended power stages clip asymmetrically (even harmonics); push-pull stages clip symmetrically (odd).
- **Zollner [P]:**
  - Small-signal curves of tubes and transistors are both mostly k2. Odd orders dominate once limiting sets in.
  - "Tube = k2, transistor = k3" is a myth.
  - In cascaded Fender stages the k2 terms can cancel, leaving k3 dominant.

### B6. Antialiasing

- **Yeh:** 8× oversampling is enough for high-quality distortion.
- **Parker, Zavalishin & Le Bivic, DAFx-16 [P], hard clipper at gain 10, rates needed for a similar SNR:**
  - no antialiasing: 12×;
  - 1st-order ADAA: 4×;
  - 2nd-order ADAA: 3×.
- **1st-order ADAA:** y[n] = (F0(x[n]) − F0(x[n−1]))/(x[n] − x[n−1]). Use f(mean) when the two inputs are nearly equal. It adds half a sample of delay. For tanh, F0 = log cosh.
- **Further work:** Bilbao et al. 2017 (higher-order ADAA); Esqueda, Bilbao & Välimäki 2016 (BLAMP correction for hard clipping).

## References (URLs)

1. Paiva, Pakarinen, Välimäki, "Acoustics and modeling of pickups", JAES 60(10) 2012: https://aaltodoc.aalto.fi/server/api/core/bitstreams/92759095-3b64-4a0f-bf29-8a3617b9d92c/content
2. Tillman, "Response Effects of Guitar Pickup Position and Width": http://www.till.com/articles/PickupResponse/
3. Mohamad, Dixon, Harte, ICASSP 2017: https://webspace.eecs.qmul.ac.uk/s.e.dixon/pub/2017/MohamadHarteDixon-ICASSP2017.pdf
4. Zollner, "Physics of the Electric Guitar" (PotEG) chapters on gitec-forum-eng.de (§2, §5.4.4, §5.5.2, §5.5.4, §5.9, §6, §9.4, §10.1, §10.10.7, §11), e.g. https://www.gitec-forum-eng.de/wp-content/uploads/2019/03/poteg-11-loudspeakers.pdf
5. Zollner, DAGA 2012: https://pub.dega-akustik.de/DAGA_2012/data/articles/000481.pdf
6. Lemme, "The Secrets of Electric Guitar Pickups": http://buildyourguitar.com/resources/lemme/ (table: …/lemme/table.htm)
7. Horton & Moore, Am. J. Phys. 77(2) 2009, DOI 10.1119/1.2990663
8. Novak et al., DAFx-16: https://dafx16.vutbr.cz/dafxpapers/34-DAFx-16_paper_34-PN.pdf
9. Meiser, Strat electrics (GITEC): https://gitec-forum.de/wp-content/uploads/Strat_Elektrik_Teil1.pdf ; bcm6: https://www.gitec-forum-eng.de/wp-content/uploads/2019/05/bcm6_interactions-pickup_pot_and_cable.pdf
10. Pakarinen & Yeh, CMJ 33(2) 2009, DOI 10.1162/comj.2009.33.2.85
11. Yeh & Smith, DAFx-06: https://ccrma.stanford.edu/~dtyeh/papers/yeh06_dafx.pdf
12. Yeh PhD thesis 2009: https://ccrma.stanford.edu/~dtyeh/papers/DavidYehThesissinglesided.pdf
13. Yeh, Abel, Smith, DAFx-07 (pedals): https://ccrma.stanford.edu/~dtyeh/papers/yeh07_dafx_distortion.pdf ; (diode clipper ODE): https://ccrma.stanford.edu/~dtyeh/papers/yeh07_dafx_clipode.pdf
14. Yeh, Bank, Karjalainen, DAFx-08 (cabinet): https://ccrma.stanford.edu/~dtyeh/papers/yeh08_dafx_cab.pdf
15. Parker, Zavalishin, Le Bivic, DAFx-16 (ADAA): https://dafx.de/paper-archive/2016/dafxpapers/20-DAFx-16_paper_41-PN.pdf
16. Bilbao, Esqueda, Parker, Välimäki, IEEE SPL 24(7) 2017, DOI 10.1109/LSP.2017.2675541
17. Esqueda, Bilbao, Välimäki, IEEE TSP 64(20) 2016, DOI 10.1109/TSP.2016.2585091
18. Mačák & Schimmel, DAFx-11: https://www.dafx.de/paper-archive/2011/Papers/05_e.pdf
19. Kuehnel, Bassman 5F6-A summary: https://robrobinette.com/images/Guitar/Bassman/Fender_Bassman_5F6-A_Circuit_Kuehnel.pdf
20. Celestion V30: https://celestion.com/product/vintage-30/ ; G12M: https://celestion.com/product/g12m-greenback/ ; Jensen P12R: https://www.jensentone.com/specification-sheet/41
