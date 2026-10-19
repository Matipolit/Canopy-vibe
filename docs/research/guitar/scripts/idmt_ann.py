#!/usr/bin/env python3
"""Summarise IDMT-SMT-Guitar XML annotations. Usage: idmt_ann.py [--events] file.xml ..."""
import sys, os
import xml.etree.ElementTree as ET


def events(path):
    root = ET.parse(path).getroot()
    out = []
    for ev in root.iter("event"):
        d = {c.tag: (c.text or "").strip() for c in ev}
        out.append(d)
    g = {c.tag: (c.text or "").strip() for c in root.find("globalParameter")}
    return g, out


def main():
    args = sys.argv[1:]
    show = False
    if args and args[0] == "--events":
        show = True
        args = args[1:]
    for p in args:
        g, evs = events(p)
        name = os.path.basename(p)
        if not evs:
            print(name, "NO EVENTS")
            continue
        on = [float(e["onsetSec"]) for e in evs]
        off = [float(e["offsetSec"]) for e in evs]
        durs = [b - a for a, b in zip(on, off)]
        uniq = lambda k: sorted(set(e.get(k, "?") for e in evs))
        print(f"{name}\tmodel={g.get('instrumentModel')}\tpickup={g.get('pickUpSetting')}\tn={len(evs)}\tpitch={uniq('pitch')}\tstr={uniq('stringNumber')}\tfret={uniq('fretNumber')}\texc={uniq('excitationStyle')}\texpr={uniq('expressionStyle')}\tdur={min(durs):.2f}-{max(durs):.2f}\tspan={on[0]:.2f}-{off[-1]:.2f}")
        if show:
            for e in evs:
                print("   ", e.get("onsetSec"), e.get("offsetSec"), e.get("pitch"), "s" + e.get("stringNumber", "?"), "f" + e.get("fretNumber", "?"), e.get("excitationStyle"), e.get("expressionStyle"))


if __name__ == "__main__":
    main()
