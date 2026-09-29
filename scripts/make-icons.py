#!/usr/bin/env python3
"""Render BackDown's desktop app icons from the brand mark, with Pillow only (no rsvg, Inkscape or ImageMagick).

    python3 scripts/make-icons.py

Reads marketing/public/brand/backdown-mark.svg (one path: M, H, V, A and Z commands, even-odd fill) and writes
crates/backdown-cli/assets/icons/, which the binary embeds (desktop.rs):

  backdown-<N>.png         16 24 32 48 64 128 256 512: the ivory tile with the orange mark, for Linux (hicolor)
                           and Windows (.ico); full-bleed up to 32 px like the favicon, with a margin above
  backdown.svg             the same tile, scalable (hicolor/scalable)
  backdown-macos-<N>.png   16 32 64 128 256 512 1024: Apple's icon grid (an 824/1024 tile with a soft shadow), for
                           the .icns

Colours from marketing/brand/README.md: orange #d8432b, ivory #f5f3ec, charcoal #17161b. The output is committed;
run this again only when the mark or the design changes.
"""
import math
import re
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
MARK = ROOT / 'marketing/public/brand/backdown-mark.svg'
OUT = ROOT / 'crates/backdown-cli/assets/icons'
IVORY, ORANGE, CHARCOAL = (0xf5, 0xf3, 0xec), (0xd8, 0x43, 0x2b), (0x17, 0x16, 0x1b)
MARK_W, MARK_H = 80.0, 60.0  # the mark's viewBox


def subpaths(d):
    """The path as closed polygons in viewBox units (arcs flattened finely)."""
    tokens = re.findall(r'[MLHVAZmlhvaz]|-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?', d)
    i, out, cur, x, y = 0, [], [], 0.0, 0.0

    def num():
        nonlocal i
        i += 1
        return float(tokens[i - 1])

    cmd = None
    while i < len(tokens):
        if re.fullmatch(r'[A-Za-z]', tokens[i]):
            cmd = tokens[i]
            i += 1
        if cmd not in 'MLHVAZ':
            raise SystemExit(f'unsupported path command {cmd} (only absolute M L H V A Z)')
        if cmd == 'M':
            if cur:
                out.append(cur)
            x, y = num(), num()
            cur = [(x, y)]
            cmd = 'L'
        elif cmd == 'L':
            x, y = num(), num()
            cur.append((x, y))
        elif cmd == 'H':
            x = num()
            cur.append((x, y))
        elif cmd == 'V':
            y = num()
            cur.append((x, y))
        elif cmd == 'A':
            rx, ry, phi, fa, fs, x2, y2 = (num() for _ in range(7))
            cur.extend(arc(x, y, rx, ry, phi, int(fa), int(fs), x2, y2))
            x, y = x2, y2
        elif cmd == 'Z':
            out.append(cur)
            cur = []
    if cur:
        out.append(cur)
    return out


def arc(x1, y1, rx, ry, phi, fa, fs, x2, y2):
    """SVG endpoint arc -> points (SVG 1.1 implementation notes F.6.5)."""
    c, s = math.cos(math.radians(phi)), math.sin(math.radians(phi))
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p, y1p = c * dx + s * dy, -s * dx + c * dy
    rx, ry = abs(rx), abs(ry)
    lam = x1p ** 2 / rx ** 2 + y1p ** 2 / ry ** 2
    if lam > 1:
        rx, ry = rx * math.sqrt(lam), ry * math.sqrt(lam)
    num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p
    den = rx * rx * y1p * y1p + ry * ry * x1p * x1p
    coef = math.sqrt(max(0.0, num / den)) * (-1 if fa == fs else 1)
    cxp, cyp = coef * rx * y1p / ry, -coef * ry * x1p / rx
    cx, cy = c * cxp - s * cyp + (x1 + x2) / 2, s * cxp + c * cyp + (y1 + y2) / 2

    def angle(ux, uy, vx, vy):
        a = math.atan2(ux * vy - uy * vx, ux * vx + uy * vy)
        return a

    t1 = angle(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dt = angle((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if not fs and dt > 0:
        dt -= 2 * math.pi
    elif fs and dt < 0:
        dt += 2 * math.pi
    n = max(8, int(abs(dt) * max(rx, ry) * 4))
    return [(c * rx * math.cos(t1 + dt * k / n) - s * ry * math.sin(t1 + dt * k / n) + cx,
             s * rx * math.cos(t1 + dt * k / n) + c * ry * math.sin(t1 + dt * k / n) + cy) for k in range(1, n + 1)]


def mark_mask(polys, size, box):
    """An L mask of `size` with the mark (even-odd) in box = (x, y, w) (height from the aspect ratio)."""
    bx, by, bw = box
    k = bw / MARK_W
    acc = Image.new('L', size, 0)
    for p in polys:
        layer = Image.new('L', size, 0)
        ImageDraw.Draw(layer).polygon([(bx + px * k, by + py * k) for px, py in p], fill=255)
        acc = ImageChops.difference(acc, layer)  # masks are 0/255: difference is XOR, i.e. even-odd
    return acc


def render(polys, n, macos):
    ss = max(4, math.ceil(2048 / n))  # supersampling
    s = n * ss
    img = Image.new('RGBA', (s, s), (0, 0, 0, 0))
    if macos:  # Apple's grid: an 824 px tile in 1024, a soft shadow below it
        tile, off = s * 824 / 1024, s * 100 / 1024
        radius = tile * 0.2237
        shadow = Image.new('L', (s, s), 0)
        dy = s * 12 / 1024
        ImageDraw.Draw(shadow).rounded_rectangle((off, off + dy, off + tile, off + tile + dy), radius, fill=int(255 * 0.30))
        shadow = shadow.filter(ImageFilter.GaussianBlur(s * 14 / 1024))
        img.paste((0, 0, 0, 255), (0, 0), shadow)
        mark_w = tile * 0.70
    else:
        margin = 0 if n <= 32 else round(n * 0.0625) * ss
        tile, off = s - 2 * margin, margin
        radius = tile * (0.28 if n <= 32 else 0.225)
        mark_w = tile * 0.75
    draw = ImageDraw.Draw(img)
    # a faint charcoal edge keeps the ivory tile visible on light panels and docks
    edge = max(ss, round(tile / 96))
    draw.rounded_rectangle((off, off, off + tile - 1, off + tile - 1), radius, fill=CHARCOAL + (40,))
    draw.rounded_rectangle((off + edge, off + edge, off + tile - 1 - edge, off + tile - 1 - edge), radius - edge, fill=IVORY + (255,))
    mark_h = mark_w * MARK_H / MARK_W
    mask = mark_mask(polys, (s, s), (off + (tile - mark_w) / 2, off + (tile - mark_h) / 2, mark_w))
    img.paste(ORANGE + (255,), (0, 0), mask)
    return img.convert('RGBa').resize((n, n), Image.LANCZOS).convert('RGBA')


def svg(d):
    """The standard tile as a scalable SVG (the 128 px design)."""
    n, margin = 128, 8
    tile = n - 2 * margin
    mark_w = tile * 0.75
    mark_h = mark_w * MARK_H / MARK_W
    k = mark_w / MARK_W
    x, y = margin + (tile - mark_w) / 2, margin + (tile - mark_h) / 2
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}">'
            f'<rect x="{margin + .5}" y="{margin + .5}" width="{tile - 1}" height="{tile - 1}" rx="{tile * .225:.2f}" '
            f'fill="#f5f3ec" stroke="#17161b" stroke-opacity=".16"/>'
            f'<path transform="translate({x:g} {y:g}) scale({k:g})" fill="#d8432b" fill-rule="evenodd" d="{d}"/></svg>\n')


def main():
    d = re.search(r'\sd="([^"]+)"', MARK.read_text()).group(1)
    polys = subpaths(d)
    OUT.mkdir(parents=True, exist_ok=True)
    for n in (16, 24, 32, 48, 64, 128, 256, 512):
        render(polys, n, False).save(OUT / f'backdown-{n}.png', optimize=True)
    for n in (16, 32, 64, 128, 256, 512, 1024):
        render(polys, n, True).save(OUT / f'backdown-macos-{n}.png', optimize=True)
    (OUT / 'backdown.svg').write_text(svg(d))
    total = sum(p.stat().st_size for p in OUT.iterdir())
    print(f'{OUT.relative_to(ROOT)}: {len(list(OUT.iterdir()))} files, {total:,} bytes')


if __name__ == '__main__':
    main()
