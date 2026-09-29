#!/usr/bin/env python3
"""Add (or change) one string in every i18n/<lang>.json, keeping each file's order and formatting.

    python3 marketing/tools/i18n-add.py KEY --after EXISTING_KEY en="Docs" de="Doku" ...

Every language must be given, so a new key never ships half-translated (build.py refuses that anyway).
"""
import argparse
import json
import sys
from pathlib import Path

I18N = Path(__file__).resolve().parent.parent / 'i18n'


def dump(table):
    return json.dumps(table, ensure_ascii=False, indent=2) + '\n'


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('key')
    ap.add_argument('--after', required=True, help='put the new key after this one')
    ap.add_argument('values', nargs='+', help='lang="text" for every language')
    a = ap.parse_args()
    values = dict(v.split('=', 1) for v in a.values)
    files = sorted(I18N.glob('*.json'))
    langs = {f.stem for f in files}
    if set(values) != langs:
        sys.exit(f'give every language: missing {sorted(langs - set(values))}, unknown {sorted(set(values) - langs)}')
    for f in files:
        raw = f.read_text(encoding='utf-8')
        table = json.loads(raw)
        if dump(table) != raw:
            sys.exit(f'{f.name} is not in the standard layout (2-space indent, UTF-8); not touching it')
        if a.after not in table:
            sys.exit(f'{f.name} has no {a.after}')
        out = {}
        for k, v in table.items():
            if k == a.key:
                continue
            out[k] = v
            if k == a.after:
                out[a.key] = values[f.stem]
        f.write_text(dump(out), encoding='utf-8')
        print(f'{f.name}: {a.key} = {values[f.stem]}')


if __name__ == '__main__':
    main()
