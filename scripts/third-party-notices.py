#!/usr/bin/env python3
"""Regenerate crates/backdown-cli/THIRD-PARTY-NOTICES.txt, which ships in every release archive.

Keeps the hand-written parts (the browser viewers, SQLite, and the licence texts at the end) and
rebuilds the rest from the source of truth:
- the Rust crates compiled into `backdown` on any platform (`cargo tree --target all -e normal`),
- the runtime pieces the release toolchain links in statically (musl on Linux, mingw-w64 and LLVM's
  compiler-rt on Windows; zig's compiler_rt everywhere).

Run it before a release (build-local.sh checks it's current). `--check` exits 1 if the file is out
of date. Crates whose licence isn't one of the usual permissive ones are listed on stderr, so a
copyleft dependency can't slip into the downloads unnoticed.
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "crates", "backdown-cli", "THIRD-PARTY-NOTICES.txt")
PERMISSIVE = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "0BSD",
              "Unicode-3.0", "Unicode-DFS-2016", "CC0-1.0", "Unlicense", "BSL-1.0",
              "Apache-2.0 WITH LLVM-exception", "MIT-0", "bzip2-1.0.6"}

RUNTIME = """Runtime libraries linked into the release builds
------------------------------------------------
Linux builds are static: they include musl libc (MIT; its licence text follows the Rust crate list).
Windows builds include startup and runtime support code from mingw-w64 (Zope Public License 2.1 and
public domain) and LLVM's compiler-rt (Apache-2.0 WITH LLVM-exception), linked by the Zig toolchain.
All builds include compiler_rt from the Zig project (MIT). macOS builds link the system's libSystem.
"""

MUSL = """==== musl libc ====

Copyright © 2005-2020 Rich Felker, et al.

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

"""


def crates():
    out = subprocess.run(
        ["cargo", "tree", "-p", "backdown-cli", "--target", "all", "-e", "normal",
         "--prefix", "none", "--format", "{p}|{l}", "--locked"],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout
    seen = {}
    for line in out.splitlines():
        line = re.sub(r" \((\*|proc-macro)\)", "", line).strip()
        if not line or line.startswith("backdown"):
            continue
        pkg, _, lic = line.partition("|")
        name, _, ver = pkg.partition(" v")
        # the path of a workspace crate after its version: drop it
        ver = ver.split(" ")[0]
        seen[(name, ver)] = lic.strip() or "see the crate's own licence file"
    return sorted(seen.items(), key=lambda kv: (kv[0][0].lower(), kv[0][1]))


def unusual(lic):
    parts = [p.strip("() ") for p in re.split(r"\s+OR\s+|/", lic)]
    # an OR-licence is fine if any choice is permissive; AND needs all of them
    for choice in parts:
        needed = [x.strip("() ") for x in re.split(r"\s+AND\s+", choice)]
        if all(x in PERMISSIVE for x in needed):
            return False
    return True


def main():
    old = open(OUT, encoding="utf-8").read()
    head = old[: old.index("Rust crates (")]
    tail = old[old.index("==== pdf.js ===="):]
    # the header, with the runtime section once
    if "Runtime libraries linked" not in head:
        head = head.rstrip("\n") + "\n\n" + RUNTIME + "\n"
    list_ = crates()
    body = f"Rust crates ({len(list_)})\n" + "-" * (len(f"Rust crates ({len(list_)})")) + "\n"
    body += "".join(f"{n} {v}: {lic}\n" for (n, v), lic in list_)
    if "==== musl libc ====" not in tail:
        tail = MUSL + tail
    new = head + body + "\n\n" + tail
    odd = [(n, v, lic) for (n, v), lic in list_ if unusual(lic)]
    for n, v, lic in odd:
        print(f"check the licence: {n} {v}: {lic}", file=sys.stderr)
    if "--check" in sys.argv:
        if new != old:
            print("THIRD-PARTY-NOTICES.txt is out of date: run scripts/third-party-notices.py", file=sys.stderr)
            sys.exit(1)
        return
    open(OUT, "w", encoding="utf-8").write(new)
    print(f"{OUT}: {len(list_)} crates" + (f", {len(odd)} to check by hand" if odd else ""))


if __name__ == "__main__":
    main()
