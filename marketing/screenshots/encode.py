#!/usr/bin/env python3
"""out/<lang>/<slide>-<theme>.png (2x captures from capture.mjs) -> ../public/shots/<lang>/<slide>-<theme>-<width>.webp for the slideshow srcset."""
from pathlib import Path
from PIL import Image

HERE = Path(__file__).resolve().parent
DEST = HERE.parent / 'public' / 'shots'
WIDTHS = (2400, 1600, 960)

for png in sorted((HERE / 'out').glob('*/*.png')):
    im = Image.open(png).convert('RGB')
    (DEST / png.parent.name).mkdir(parents=True, exist_ok=True)
    for w in WIDTHS:
        out = DEST / png.parent.name / f'{png.stem}-{w}.webp'
        im.resize((w, round(im.height * w / im.width)), Image.LANCZOS).save(out, 'WEBP', quality=82, method=5)
    print(f'{png.parent.name}/{png.stem}: {", ".join(str(w) for w in WIDTHS)}')
