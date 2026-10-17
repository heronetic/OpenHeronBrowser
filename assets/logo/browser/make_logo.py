# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0

"""OpenHeronBrowser logo: a wireframe egg (the web globe, egg-shaped) cracked open,
with the Heronetic heron rising out of it and the top of the shell tipping away.

    python3 make_logo.py     writes the SVG variants into ./out/
"""
import math
import os

GOLD, BLACK, WHITE = "#F5C842", "#0B0B0C", "#FFFFFF"

# ---- egg ---------------------------------------------------------------------------
CX, TOP, BOT, W = 256, 132, 462, 116    # egg spans TOP..BOT, half-width W (ratio ~0.7, like a real egg)
CRACK = TOP + 0.40 * (BOT - TOP)        # the crack runs across here
E = 0.16                                # wider toward the bottom


def hw(y):
    u = max(-1.0, min(1.0, (y - TOP) / (BOT - TOP) * 2 - 1))
    return W * math.sqrt(max(0.0, 1 - u * u)) * (1 + E * u)


def crack(teeth=6, up=7, down=14):
    xl, xr = CX - hw(CRACK), CX + hw(CRACK)
    n = teeth * 2
    return [(xl + (xr - xl) * i / n, CRACK + (0 if i in (0, n) else (down if i % 2 else -up))) for i in range(n + 1)]


def path(points, close=True):
    return "M" + " L".join(f"{x:.1f} {y:.1f}" for x, y in points) + (" Z" if close else "")


def lower_shell():
    steps = 64
    right = [(CX + hw(y), y) for y in (CRACK + (BOT - CRACK) * k / steps for k in range(1, steps + 1))]
    left = [(CX - hw(y), y) for y in (BOT - (BOT - CRACK) * k / steps for k in range(1, steps))]
    return path(crack() + right + left)


def cap_shell():
    """The broken-off top: the crack (right to left) and then the dome back over to the right."""
    steps = 48
    dome = [(CX - hw(y), y) for y in (CRACK - (CRACK - TOP) * k / steps for k in range(1, steps + 1))]
    dome += [(CX + hw(y), y) for y in (TOP + (CRACK - TOP) * k / steps for k in range(1, steps))]
    return path(list(reversed(crack())) + dome)


def meridian(s):
    ys = [CRACK - 24 + (BOT - CRACK + 24) * k / 60 for k in range(61)]
    return path([(CX + s * hw(y), y) for y in ys], close=False)


def parallel(y, tilt=0.18, front=True):
    rx, ry = hw(y), hw(y) * tilt
    sweep = 0 if front else 1
    return f"M{CX - rx:.1f} {y:.1f} A{rx:.1f} {ry:.1f} 0 0 {sweep} {CX + rx:.1f} {y:.1f}"


# ---- heron (head from the Heronetic brand mark, facing left) -------------------------
def heron_head(x, y, s, fg):
    def T(px, py):
        return x + (px - 144.5) * s, y + (py - 40) * s
    hx, hy = T(144.5, 40); r = 108 * s
    ex, ey = T(162, 22); er = 26 * s
    b1, b2, b3 = T(96, -6), T(104, 100), T(-250, 46)        # beak, a little longer and slimmer than the mark's
    p1, p2 = T(226, -26), T(392, -76)
    return (f'<path fill="{fg}" fill-rule="evenodd" d="M{hx - r:.1f} {hy:.1f} a{r:.1f} {r:.1f} 0 1 0 {2 * r:.1f} 0 '
            f'a{r:.1f} {r:.1f} 0 1 0 {-2 * r:.1f} 0 Z M{ex - er:.1f} {ey:.1f} a{er:.1f} {er:.1f} 0 1 0 {2 * er:.1f} 0 '
            f'a{er:.1f} {er:.1f} 0 1 0 {-2 * er:.1f} 0 Z"/>'
            f'<path fill="{fg}" d="M{b1[0]:.1f} {b1[1]:.1f} L{b2[0]:.1f} {b2[1]:.1f} L{b3[0]:.1f} {b3[1]:.1f} Z"/>'
            f'<path d="M{p1[0]:.1f} {p1[1]:.1f} L{p2[0]:.1f} {p2[1]:.1f}" stroke="{fg}" stroke-width="{30 * s:.1f}" '
            f'stroke-linecap="round" fill="none"/>')


def logo(fg=GOLD, bg=BLACK, tile=True, detail="full", uid="a", round_=False):
    """round_: a circle instead of the rounded square (profile pictures, round icons).
    detail: "full" (app icon, large sizes), "small" (32-48 px: fewer wires, no cap)
    or "tiny" (16-24 px favicon: outline, crack, neck and head only)."""
    full, tiny = detail == "full", detail == "tiny"
    hx, hy, hs = 214, 70, 0.25                               # head center and scale
    # an S-shaped heron neck from inside the egg up to the head
    neck = (f'<path d="M268 {CRACK + 50:.0f} C 266 {CRACK - 12:.0f}, 300 {CRACK - 40:.0f}, 286 {CRACK - 84:.0f} '
            f'C 274 {CRACK - 122:.0f}, 222 {hy + 52}, {hx + 10} {hy + 10}" fill="none" stroke="{fg}" '
            f'stroke-width="{17 if full else (34 if tiny else 22)}" stroke-linecap="round" clip-path="url(#above-{uid})"/>')
    above = [(-80, -80), (600, -80), (600, CRACK)] + list(reversed(crack())) + [(-80, CRACK)]
    neck = f'<clipPath id="above-{uid}"><path d="{path(above)}"/></clipPath>' + neck
    back_rim = (f'<path d="{parallel(CRACK, tilt=0.2, front=False)}" fill="none" stroke="{fg}" '
                f'stroke-width="6" opacity="0.45"/>') if full else ""
    shell = lower_shell()
    if full:
        wires = "".join(f'<path d="{meridian(s)}"/>' for s in (-0.7, -0.36, 0, 0.36, 0.7))
        wires += "".join(f'<path d="{parallel(CRACK + (BOT - CRACK) * f)}"/>' for f in (0.24, 0.52, 0.78))
        wire_w = 7
    elif tiny:
        wires, wire_w = "", 0
    else:
        wires = "".join(f'<path d="{meridian(s)}"/>' for s in (-0.5, 0, 0.5))
        wires += f'<path d="{parallel(CRACK + (BOT - CRACK) * 0.5)}"/>'
        wire_w = 12
    cap = ""
    if full:
        # the top of the shell, tipped up off the right side of the crack: it just broke off
        ccx, ccy = CX, (TOP + CRACK) / 2 + 8                 # middle of the cap piece
        tx, ty, rot, sc = 404, 232, 52, 0.44
        cap = (f'<g transform="translate({tx} {ty}) rotate({rot}) scale({sc}) translate({-ccx} {-ccy})">'
               f'<path d="{cap_shell()}" fill="{bg}" fill-opacity="{1 if tile else 0}" stroke="{fg}" stroke-width="26" '
               f'stroke-linejoin="round"/></g>')
    shell_fill = bg if tile else "none"
    body = (back_rim + neck +
            f'<path d="{shell}" fill="{bg}" fill-opacity="{1 if tile else 0}"/>'
            f'<clipPath id="shell-{uid}"><path d="{shell}"/></clipPath>'
            f'<g clip-path="url(#shell-{uid})" fill="none" stroke="{fg}" stroke-width="{wire_w}" stroke-linecap="round" '
            f'opacity="0.8">{wires}</g>'
            f'<path d="{shell}" fill="none" stroke="{fg}" stroke-width="{14 if full else (36 if tiny else 20)}" stroke-linejoin="round"/>'
            + heron_head(hx, hy, hs if full else (0.36 if tiny else 0.3), fg) + cap)
    if round_:
        rect = f'<circle cx="256" cy="256" r="256" fill="{bg}"/>'
        # pull everything in so the beak, cap and egg sit inside the circle with a margin
        shift = ("translate(256 262) scale(0.8) translate(-278 -262)" if full else
                 "translate(256 262) scale(0.84) translate(-266 -262)")
    else:
        rect = f'<rect width="512" height="512" rx="114" fill="{bg}"/>' if tile else ""
        shift = "translate(-22 6)" if full else "translate(-10 4)"     # optical centring of heron + egg + cap
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" role="img" aria-label="OpenHeronBrowser">'
            f'<title>OpenHeronBrowser</title>{rect}<g transform="{shift}">{body}</g></svg>')


if __name__ == "__main__":
    os.makedirs("out", exist_ok=True)
    variants = {
        "ohb-icon.svg": logo(),                                   # app icon: gold on black tile
        "ohb-icon-small.svg": logo(detail="small", uid="s"),      # 32-48 px
        "ohb-favicon.svg": logo(detail="tiny", uid="t"),          # 16-24 px
        "ohb-round.svg": logo(round_=True, uid="r"),              # round: profile pictures, round icons
        "ohb-round-small.svg": logo(detail="small", uid="rs", round_=True),
        "ohb-round-favicon.svg": logo(detail="tiny", uid="rt", round_=True),
    }
    for name, svg in variants.items():
        open(os.path.join("out", name), "w").write(svg)
    print("wrote", ", ".join(variants))
