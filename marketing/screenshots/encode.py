#!/usr/bin/env python3
"""out/<slide>-<theme>.png (2x captures from capture.mjs) -> ../public/shots/<slide>-<theme>-<width>.webp for the slideshow srcset."""
from pathlib import Path
from PIL import Image

HERE = Path(__file__).resolve().parent
DEST = HERE.parent / 'public' / 'shots'
WIDTHS = (1600, 960)

DEST.mkdir(exist_ok=True)
for png in sorted((HERE / 'out').glob('*.png')):
    im = Image.open(png).convert('RGB')
    for w in WIDTHS:
        out = DEST / f'{png.stem}-{w}.webp'
        im.resize((w, round(im.height * w / im.width)), Image.LANCZOS).save(out, 'WEBP', quality=82, method=6)
        print(f'{out.relative_to(HERE.parent)}  {out.stat().st_size // 1024} KB')
