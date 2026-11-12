#!/usr/bin/env python
"""Anvil logo, generated entirely from code.

    python build_anvil.py      # writes the SVGs next to this file, PNG exports into png/, and preview.png

Every shape is a polygon with per-vertex corner radii, so the artwork is straight lines and circular
arcs only: no gradients, no strokes, no fonts. The master lives on a 512 grid. The 16 px and 32 px
versions are separate drawings of the same idea, fitted to the pixel grid, not downscales.
"""
import io
import math
from pathlib import Path

import resvg_py
from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
SLATE, STEEL, ORANGE, GOLD = '#1E2630', '#E9EDF1', '#FF6A1A', '#FFB020'
SIZES = (16, 32, 64, 128, 256, 512, 1024)


# ----------------------------------------------------------------------------- geometry helpers
def f(v):
    s = f'{v:.2f}'.rstrip('0').rstrip('.')
    return '0' if s in ('-0', '') else s


def rpoly(verts):
    """Closed path through (x, y, r) vertices; each corner is cut by a circular arc of radius r."""
    n, corners = len(verts), []
    for i, (x, y, r) in enumerate(verts):
        px, py, _ = verts[i - 1]
        nx, ny, _ = verts[(i + 1) % n]
        l1, l2 = math.hypot(px - x, py - y), math.hypot(nx - x, ny - y)
        u1, u2 = ((px - x) / l1, (py - y) / l1), ((nx - x) / l2, (ny - y) / l2)
        if r <= 0:
            corners.append(((x, y), (x, y), 0, 0, 0.0))
            continue
        ang = math.acos(max(-1.0, min(1.0, u1[0] * u2[0] + u1[1] * u2[1])))
        tl = r / math.tan(ang / 2)
        sweep = 1 if (-u1[0]) * u2[1] - (-u1[1]) * u2[0] > 0 else 0
        corners.append(((x + u1[0] * tl, y + u1[1] * tl), (x + u2[0] * tl, y + u2[1] * tl), r, sweep, tl))
    for i in range(n):                                   # design-time check: fillets must fit their edges
        (x, y, _), (nx, ny, _) = verts[i], verts[(i + 1) % n]
        if corners[i][4] + corners[(i + 1) % n][4] > math.hypot(nx - x, ny - y) + 1e-6:
            raise ValueError(f'corner radii overlap on edge {i}: {verts[i]} -> {verts[(i + 1) % n]}')
    d = []
    for i, (a, b, r, sweep, _) in enumerate(corners):
        d.append(('M' if i == 0 else 'L') + f'{f(a[0])} {f(a[1])}')
        if r > 0:
            d.append(f'A{f(r)} {f(r)} 0 0 {sweep} {f(b[0])} {f(b[1])}')
    return ' '.join(d) + ' Z'


def anvil_vertices(ox, oy, L, H, T, cx, ww, fw, ft, Ru, Rl, Rh, hy, tip, rt, rc, notch=None):
    """Side view, horn to the left. Local origin: horn tip x, face y.
    L length, H height, T top-block thickness, cx body axis, ww waist half-width, fw foot half-width,
    ft foot thickness, Ru/Rl/Rh fillets (under heel, foot flare, under horn), hy where the horn's underside
    meets the waist line, tip blunt-tip height, rt/rc corner radii, notch (half-width bottom, top, depth, r)."""
    v = [(0, 0, rt), (L, 0, rc), (L, T, rc), (cx + ww, T, Ru), (cx + ww, H - ft, Rl), (cx + fw, H - ft, rc), (cx + fw, H, rc)]
    if notch:
        nb, nt, nd, nr = notch
        v += [(cx + nb, H, rc * 0.5), (cx + nt, H - nd, nr), (cx - nt, H - nd, nr), (cx - nb, H, rc * 0.5)]
    v += [(cx - fw, H, rc), (cx - fw, H - ft, rc), (cx - ww, H - ft, Rl), (cx - ww, hy, Rh), (0, tip, rt)]
    return [(ox + x, oy + y, r) for x, y, r in v]


def chevron_vertices(x, y, h, th, r):
    """A '>' with 45 degree arms and horizontal cuts, so it sits parallel to the anvil face."""
    m = h / 2
    return [(x, y, r), (x + th, y, r), (x + th + m, y + m, r), (x + th, y + h, r), (x, y + h, r), (x + m, y + m, r * 0.5)]


def square(x, y, s, r):
    return [(x, y, r), (x + s, y, r), (x + s, y + s, r), (x, y + s, r)]


# ----------------------------------------------------------------------------- the drawings
# sparks: (dx, dy, size, colour) as centre offsets from the chevron tip; a rising arc of three plus one stray.
# With the default placement every edge lands on a multiple of 4, so they stay crisp from 128 px up.
SPARKS = [(40, -28, 28, 'gold'), (76, -52, 20, 'orange'), (100, -64, 12, 'gold'), (92, -8, 12, 'gold')]


def spec_full(sparks=SPARKS, ax=62, ay=238, chev_dx=-16):
    """Master art on the 512 grid (tile coordinates). The art sits a little left of and above the box-centred
    position: the horn is long but light, so the ink's centre of mass is right of and below the bounding box's."""
    a = dict(L=356, H=176, T=56, cx=218, ww=50, fw=107, ft=30, Ru=32, Rl=46, Rh=46, hy=62, tip=12, rt=5.5, rc=6,
             notch=(40, 24, 13, 11))
    ch_h, ch_th, gap = 108, 46, 26
    cx0 = ax + a['cx'] + chev_dx - (ch_th + ch_h / 2) / 2
    tipx, tipy = cx0 + ch_th + ch_h / 2, ay - gap - ch_h / 2
    sq = [(tipx + dx - s / 2, tipy + dy - s / 2, s, role) for dx, dy, s, role in sparks]
    return dict(grid=512, tile_r=112, anvil=(ax, ay, a), chev=(cx0, ay - gap - ch_h, ch_h, ch_th, 4.5), sparks=sq, spark_r=2.5)


def spec_tile32():
    a = dict(L=24, H=12, T=4, cx=15, ww=3, fw=7, ft=2, Ru=2, Rl=3, Rh=2.9, hy=4, tip=1, rt=0.35, rc=0.5, notch=None)
    return dict(grid=32, tile_r=7, anvil=(4, 15, a), chev=(14, 5, 8, 3, 0.3),
                sparks=[(23, 6, 2, 'gold'), (26, 4, 1, 'orange'), (26, 9, 1, 'gold')], spark_r=0)


def spec_tile16():
    a = dict(L=12, H=6, T=2, cx=7.5, ww=1.5, fw=3.5, ft=2, Ru=1, Rl=1, Rh=0.6, hy=2.5, tip=1, rt=0.2, rc=0.25, notch=None)
    return dict(grid=16, tile_r=3.5, anvil=(2, 7, a), chev=(7, 2, 4, 1.75, 0.15), sparks=[(12, 2, 1, 'gold')], spark_r=0)


def spec_glyph32():
    a = dict(L=30, H=15, T=5, cx=19, ww=4, fw=9, ft=3, Ru=2.5, Rl=4, Rh=3.2, hy=5.5, tip=1, rt=0.4, rc=0.6, notch=(3.5, 2, 1, 0.8))
    return dict(grid=32, tile_r=None, anvil=(0, 15, a), chev=(13.5, 4, 9, 4, 0.35),
                sparks=[(24, 5, 3, 'gold'), (28, 2, 2, 'orange'), (29, 9, 1, 'gold')], spark_r=0)


def spec_glyph16():
    a = dict(L=16, H=8, T=3, cx=10, ww=2, fw=5, ft=2, Ru=1, Rl=2, Rh=1, hy=3.2, tip=1, rt=0.2, rc=0.3, notch=None)
    return dict(grid=16, tile_r=None, anvil=(0, 7, a), chev=(7, 1, 5, 2.2, 0.15), sparks=[(13, 1, 2, 'gold')], spark_r=0)


def shapes(spec):
    """[(role, path d)] in paint order; roles: anvil, chevron, gold, orange."""
    ax, ay, a = spec['anvil']
    out = [('anvil', rpoly(anvil_vertices(ax, ay, **a)))]
    if spec.get('chev'):
        out.append(('chevron', rpoly(chevron_vertices(*spec['chev']))))
    for x, y, s, role in spec.get('sparks', []):
        out.append((role, rpoly(square(x, y, s, spec['spark_r']))))
    return out


def bbox(spec):
    ax, ay, a = spec['anvil']
    pts = [(x, y) for x, y, _ in anvil_vertices(ax, ay, **a)]
    if spec.get('chev'):
        pts += [(x, y) for x, y, _ in chevron_vertices(*spec['chev'])]
    for x, y, s, _ in spec.get('sparks', []):
        pts += [(x, y), (x + s, y + s)]
    xs, ys = [p[0] for p in pts], [p[1] for p in pts]
    return min(xs), min(ys), max(xs), max(ys)


ON_DARK = dict(anvil=STEEL, chevron=ORANGE, gold=GOLD, orange=ORANGE)
ON_LIGHT = dict(anvil=SLATE, chevron=ORANGE, gold=GOLD, orange=ORANGE)


def body(spec, colours=None, mono=None, indent='  '):
    if mono:                                              # one compound path, one colour
        return f'{indent}<path fill="{mono}" d="{" ".join(d for _, d in shapes(spec))}"/>'
    return '\n'.join(f'{indent}<path fill="{colours[role]}" d="{d}"/>' for role, d in shapes(spec))


def doc(vb, inner, title, px=None):
    x, y, w, h = vb
    size = f' width="{f(px[0])}" height="{f(px[1])}"' if px else ''
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{f(x)} {f(y)} {f(w)} {f(h)}"{size} role="img" aria-label="{title}">\n'
            f'  <title>{title}</title>\n{inner}\n</svg>\n')


def icon_svg(spec):
    g = spec['grid']
    tile = f'  <rect width="{g}" height="{g}" rx="{f(spec["tile_r"])}" fill="{SLATE}"/>'
    return doc((0, 0, g, g), tile + '\n' + body(spec, ON_DARK), 'Anvil', (g, g))


def glyph_viewbox(spec, margin=0.03):
    if spec['grid'] != 512:                               # pixel-fitted glyphs keep their own grid
        return (0, 0, spec['grid'], spec['grid'])
    x0, y0, x1, y1 = bbox(spec)
    side = max(x1 - x0, y1 - y0) * (1 + 2 * margin)
    return ((x0 + x1 - side) / 2, (y0 + y1 - side) / 2, side, side)


def glyph_svg(spec, colours=None, mono=None):
    vb = glyph_viewbox(spec)
    px = (spec['grid'], spec['grid']) if spec['grid'] != 512 else (512, 512)
    return doc(vb, body(spec, colours, mono), 'Anvil', px)


# ----------------------------------------------------------------------------- wordmark (drawn, not typeset)
ASC = 42          # ascender above the x-height, in units where the x-height is 100


def wordmark(x0, baseline, xh, t=21):
    """'anvil' drawn as outlines: a geometric lowercase built from circles, stems and 45-ish diagonals.
    Unit space has the x-height at 100 (y = 0 top of x-height, y = 100 baseline). Returns ([path d], width)."""
    k, rs = xh / 100, 3
    out, pen = [], x0

    def P(x, y):
        return f'{f(pen + x * k)} {f(baseline + (y - 100) * k)}'

    def R(r):
        return f'{f(r * k)} {f(r * k)}'

    def poly(verts):
        return rpoly([(pen + x * k, baseline + (y - 100) * k, r * k) for x, y, r in verts])

    # a: a round bowl with the stem tangent on the right; the nick where they meet is what makes it an 'a'
    sx = 100 - t
    y1 = 50 - math.sqrt(50 ** 2 - (50 - t) ** 2)
    out.append(f'M{P(sx, 0)} L{P(100 - rs, 0)} A{R(rs)} 0 0 1 {P(100, rs)} L{P(100, 100 - rs)} A{R(rs)} 0 0 1 {P(100 - rs, 100)} '
               f'L{P(sx, 100)} L{P(sx, 100 - y1)} A{R(50)} 0 1 1 {P(sx, y1)} Z '
               f'M{P(t, 50)} A{R(50 - t)} 0 1 0 {P(sx, 50)} A{R(50 - t)} 0 1 0 {P(t, 50)} Z')
    pen += (100 + 28) * k
    # n: stem plus a half-ring arch springing from it, same construction as the 'a'
    W, Rn = 92, 46
    yn = Rn - math.sqrt(Rn ** 2 - (Rn - t) ** 2)
    out.append(f'M{P(0, rs)} A{R(rs)} 0 0 1 {P(rs, 0)} L{P(t, 0)} L{P(t, yn)} A{R(Rn)} 0 0 1 {P(W, Rn)} L{P(W, 100 - rs)} '
               f'A{R(rs)} 0 0 1 {P(W - rs, 100)} L{P(W - t + rs, 100)} A{R(rs)} 0 0 1 {P(W - t, 100 - rs)} L{P(W - t, Rn)} '
               f'A{R(Rn - t)} 0 0 0 {P(t, Rn)} L{P(t, 100 - rs)} A{R(rs)} 0 0 1 {P(t - rs, 100)} L{P(rs, 100)} '
               f'A{R(rs)} 0 0 1 {P(0, 100 - rs)} Z')
    pen += (W + 10) * k
    # v: two diagonals, flat cuts at the x-height, a 10-unit flat at the foot
    Wv, slope = 104, 0.47
    th = t / math.cos(math.atan(slope))
    out.append(poly([(0, 0, rs), (th, 0, rs), (Wv / 2, (Wv / 2 - th) / slope, 0), (Wv - th, 0, rs), (Wv, 0, rs),
                     (Wv / 2 + 5, 100, rs), (Wv / 2 - 5, 100, rs)]))
    pen += (Wv + 10) * k
    # i: stem and a square dot (the same square as the sparks)
    out.append(poly([(0, 0, rs), (t, 0, rs), (t, 100, rs), (0, 100, rs)]) + ' ' + poly(square(0, -ASC, t, rs)))
    pen += (t + 30) * k
    # l
    out.append(poly([(0, -ASC, rs), (t, -ASC, rs), (t, 100, rs), (0, 100, rs)]))
    pen += t * k
    return out, pen - x0


def lockup_svg(colours, ink):
    """Mark and wordmark share a baseline (the anvil's base); the 'l' is exactly as tall as the anvil."""
    spec = spec_full()
    ax, ay, a = spec['anvil']
    base = ay + a['H']
    xh = a['H'] * 100 / (100 + ASC)
    wx = ax + a['L'] + 0.56 * xh
    letters, ww = wordmark(wx, base, xh)
    x0, y0, x1, y1 = bbox(spec)
    x1 = wx + ww
    m = 40
    inner = body(spec, colours) + '\n' + '\n'.join(f'  <path fill="{ink}" d="{d}"/>' for d in letters)
    vb = (x0 - m, y0 - m, x1 - x0 + 2 * m, y1 - y0 + 2 * m)
    return doc(vb, inner, 'Anvil', (vb[2], vb[3])), vb


# ----------------------------------------------------------------------------- output
def raster(svg, w, h=None):
    return Image.open(io.BytesIO(bytes(resvg_py.svg_to_bytes(svg_string=svg, width=int(w), height=int(h or w))))).convert('RGBA')


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding='utf-8', newline='\n')


def main():
    full, t32, t16, g32, g16 = spec_full(), spec_tile32(), spec_tile16(), spec_glyph32(), spec_glyph16()
    variants = {                                           # name: {size class: svg}
        'icon': {512: icon_svg(full), 32: icon_svg(t32), 16: icon_svg(t16)},
        'mark-on-dark': {512: glyph_svg(full, ON_DARK), 32: glyph_svg(g32, ON_DARK), 16: glyph_svg(g16, ON_DARK)},
        'mark-on-light': {512: glyph_svg(full, ON_LIGHT), 32: glyph_svg(g32, ON_LIGHT), 16: glyph_svg(g16, ON_LIGHT)},
        'mark-mono-on-light': {512: glyph_svg(full, mono=SLATE), 32: glyph_svg(g32, mono=SLATE), 16: glyph_svg(g16, mono=SLATE)},
        'mark-mono-on-dark': {512: glyph_svg(full, mono=STEEL), 32: glyph_svg(g32, mono=STEEL), 16: glyph_svg(g16, mono=STEEL)},
    }
    for name, by in variants.items():
        write(HERE / f'{name}.svg', by[512])
        for small in (32, 16):
            write(HERE / 'small' / f'{name}-{small}.svg', by[small])
        for n in SIZES:
            (HERE / 'png' / name).mkdir(parents=True, exist_ok=True)
            raster(by[16] if n <= 16 else by[32] if n <= 32 else by[512], n).save(HERE / 'png' / name / f'{name}-{n}.png', optimize=True)
    lock = {}
    for name, colours, ink in (('lockup-on-dark', ON_DARK, STEEL), ('lockup-on-light', ON_LIGHT, SLATE)):
        svg, vb = lockup_svg(colours, ink)
        write(HERE / f'{name}.svg', svg)
        (HERE / 'png' / 'lockup').mkdir(parents=True, exist_ok=True)
        for w in (800, 1600):
            raster(svg, w, round(w * vb[3] / vb[2])).save(HERE / 'png' / 'lockup' / f'{name}-{w}.png', optimize=True)
        lock[name] = (svg, vb)
    preview(variants, lock)
    print('wrote', len(list(HERE.rglob('*.svg'))), 'SVG and', len(list((HERE / 'png').rglob('*.png'))), 'PNG files')


def preview(variants, lock):
    """One sheet for review: the icon, every variant on its background, small sizes zoomed, lockups."""
    DARK, LIGHT, PAGE, TILE, INK = (13, 17, 23), (255, 255, 255), (236, 238, 241), (30, 38, 48), (120, 126, 134)
    W = 1800
    sheet = Image.new('RGB', (W, 3000), PAGE)
    d = ImageDraw.Draw(sheet)

    def put(img, x, y, bg=None, pad=0):
        if bg is not None:
            d.rectangle([x - pad, y - pad, x + img.width + pad - 1, y + img.height + pad - 1], fill=bg)
        sheet.paste(img, (x, y), img)

    put(raster(variants['icon'][512], 480), 40, 40, LIGHT, 20)
    put(raster(variants['icon'][512], 480), 580, 40, DARK, 20)
    for i, (name, bg) in enumerate((('mark-on-dark', TILE), ('mark-on-light', LIGHT), ('mark-mono-on-dark', TILE), ('mark-mono-on-light', LIGHT))):
        x, y = 1120 + (i % 2) * 340, 20 + (i // 2) * 270
        put(raster(variants[name][512], 250), x + 25, y + 5, bg, 5)
        d.text((x + 26, y + 4), name, fill=INK)
    y = 590
    for name, bg in (('icon', LIGHT), ('icon', DARK), ('mark-on-dark', TILE), ('mark-on-light', LIGHT), ('mark-mono-on-light', LIGHT)):
        x = 40
        d.rectangle([0, y - 10, W, y + 170], fill=bg)
        for n, z in ((16, 10), (32, 5)):
            put(raster(variants[name][n], n).resize((n * z, n * z), Image.NEAREST), x, y)
            x += n * z + 30
        for n in (16, 32, 64, 128):
            src = variants[name][16] if n <= 16 else variants[name][32] if n <= 32 else variants[name][512]
            put(raster(src, n), x, y + (160 - n) // 2)
            x += n + 24
        put(raster(variants[name][512], 64).resize((160, 160), Image.NEAREST), x + 20, y)
        d.text((x + 200, y + 4), f'{name}: 16 px x10, 32 px x5 | 16, 32, 64, 128 at 1:1 | 64 px x2.5', fill=INK)
        y += 180
    y += 10
    for name, bg in (('lockup-on-dark', TILE), ('lockup-on-light', LIGHT)):
        svg, vb = lock[name]
        w = 860
        im = raster(svg, w, round(w * vb[3] / vb[2]))
        x = 20 if name == 'lockup-on-dark' else 920
        put(im, x, y, bg)
        small = raster(svg, 300, round(300 * vb[3] / vb[2]))
        put(small, x, y + im.height + 16, bg)
    sheet.crop((0, 0, W, y + im.height + small.height + 36)).save(HERE / 'preview.png', optimize=True)


if __name__ == '__main__':
    main()
