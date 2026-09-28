"""The docs' Markdown, a small and strict subset, to HTML. Standard library only; build.py uses it for docs/**/*.md.

Blocks:  ## headings (h2-h4; `{#id}` at the end sets the id), paragraphs, - / * / 1. lists (nested by indenting),
         ``` fenced code (sh/toml comments are dimmed), > quotes (`> [!NOTE]`, `[!TIP]`, `[!WARNING]`, `[!PREVIEW]`
         make callouts), | tables |, definition lists (a term line, then `: definition` lines), ---, and raw HTML
         blocks (a line starting with <div, <figure, <details, <p, <ul, <table, <picture or <!--, up to a blank line).
Inline:  `code`, **bold**, *italic*, [links](/docs/...), <kbd>…</kbd> / <br>, and {preview} (the "next preview"
         badge); everything else is escaped.
"""
import html
import re

CALLOUTS = {'NOTE': 'Note', 'TIP': 'Tip', 'WARNING': 'Careful', 'PREVIEW': 'In the next preview'}
RAW_BLOCK = re.compile(r'^<(div|figure|details|p|ul|ol|table|picture|section|aside|!--)[\s>/-]')
HEADING = re.compile(r'^(#{2,4})\s+(.*?)(?:\s+\{#([a-z0-9][a-z0-9-]*)\})?\s*$')
FENCE = re.compile(r'^(`{3,})\s*([\w+-]*)\s*$')
LIST_ITEM = re.compile(r'^( *)([-*]|\d{1,3}\.)( +)(.*)$')
TABLE_SEP = re.compile(r'^\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$')
HR = re.compile(r'^(-{3,}|\*{3,})\s*$')


def slugify(text):
    s = html.unescape(re.sub(r'<[^>]+>', '', text)).lower()
    s = re.sub(r"['’]", '', s)  # "can't" -> "cant"
    s = re.sub(r'[^a-z0-9]+', '-', s).strip('-')
    return s or 'section'


def plain(fragment):
    """text from an HTML fragment, whitespace collapsed"""
    return re.sub(r'\s+', ' ', html.unescape(re.sub(r'<[^>]+>', ' ', fragment))).strip()


# ---------- inline

CODE_SPAN = re.compile(r'(`+)(.+?)\1')
LINK = re.compile(r'\[([^\]]+)\]\(([^)\s]+)\)')
STRONG = re.compile(r'\*\*(?=\S)(.+?)(?<=\S)\*\*')
EM = re.compile(r'(?<![\w*])\*(?=\S)(.+?)(?<=\S)\*(?![\w*])')
ALLOWED_TAGS = re.compile(r'&lt;(/?kbd|br)&gt;')


def inline(text):
    codes = []

    def keep(m):
        code = m[2].strip() or m[2]
        # short ones (a path, a flag) don't break across lines; long ones may
        nw = ' class="nw"' if len(code) <= 30 else ''
        codes.append(f'<code{nw}>{html.escape(code, quote=False)}</code>')
        return f'\x00{len(codes) - 1}\x00'

    s = CODE_SPAN.sub(keep, text)
    s = html.escape(s, quote=False)
    s = ALLOWED_TAGS.sub(r'<\1>', s)
    s = s.replace('{preview}', '<span class="tag dev pv">next preview</span>')
    s = LINK.sub(lambda m: f'<a href="{m[2].replace(chr(34), "&quot;")}">{m[1]}</a>', s)
    s = STRONG.sub(r'<strong>\1</strong>', s)
    s = EM.sub(r'<em>\1</em>', s)
    return re.sub('\x00(\\d+)\x00', lambda m: codes[int(m[1])], s)


# ---------- code blocks

def dim_comments(line):
    """wrap a shell/TOML comment (a # outside quotes, at the start or after a space) in <span class="c">"""
    quote = None
    for i, ch in enumerate(line):
        if quote:
            if ch == quote:
                quote = None
        elif ch in '"\'':
            quote = ch
        elif ch == '#' and (i == 0 or line[i - 1] in ' \t'):
            return html.escape(line[:i], quote=False) + f'<span class="c">{html.escape(line[i:], quote=False)}</span>'
    return html.escape(line, quote=False)


def code_block(code, lang):
    body = '\n'.join(dim_comments(l) for l in code.split('\n')) if lang in ('sh', 'shell', 'bash', 'toml', 'powershell') \
        else html.escape(code, quote=False)
    cls = f' class="lang-{lang}"' if lang else ''
    return (f'<div class="code"><pre><code{cls}>{body}</code></pre>'
            '<button class="cbtn" type="button" hidden>Copy</button></div>')


# ---------- blocks

class Renderer:
    def __init__(self):
        self.headings = []  # (level, id, html)
        self.ids = set()

    def unique(self, base):
        i, n = base, 2
        while i in self.ids:
            i, n = f'{base}-{n}', n + 1
        self.ids.add(i)
        return i

    def blocks(self, lines):
        out, i = [], 0
        while i < len(lines):
            line = lines[i]
            if not line.strip():
                i += 1
                continue
            if m := FENCE.match(line):
                j = i + 1
                while j < len(lines) and not lines[j].startswith(m[1]):
                    j += 1
                out.append(code_block('\n'.join(lines[i + 1:j]), m[2].lower()))
                i = j + 1
                continue
            if m := HEADING.match(line):
                level, content = len(m[1]), inline(m[2])
                hid = self.unique(m[3] or slugify(content))
                self.headings.append((level, hid, content))
                out.append(f'<h{level} id="{hid}">{content}<a class="hash" href="#{hid}" aria-hidden="true" tabindex="-1">#</a></h{level}>')
                i += 1
                continue
            if HR.match(line):
                out.append('<hr>')
                i += 1
                continue
            if RAW_BLOCK.match(line):
                j = i
                while j < len(lines) and lines[j].strip():
                    j += 1
                out.append('\n'.join(lines[i:j]))
                i = j
                continue
            if line.startswith('>'):
                j = i
                while j < len(lines) and lines[j].startswith('>'):
                    j += 1
                out.append(self.quote([re.sub(r'^> ?', '', l) for l in lines[i:j]]))
                i = j
                continue
            if line.startswith('|') and i + 1 < len(lines) and TABLE_SEP.match(lines[i + 1]):
                j = i + 2
                while j < len(lines) and lines[j].startswith('|'):
                    j += 1
                out.append(self.table(lines[i], lines[i + 1], lines[i + 2:j]))
                i = j
                continue
            if LIST_ITEM.match(line) and not LIST_ITEM.match(line)[1]:
                i = self.list(lines, i, out)
                continue
            if i + 1 < len(lines) and lines[i + 1].startswith(': '):
                i = self.deflist(lines, i, out)
                continue
            j = i
            para = []
            while j < len(lines) and lines[j].strip() and not self.starts_block(lines, j):
                para.append(lines[j].strip())
                j += 1
            if not para:  # a line that looked like a block start but wasn't one
                para, j = [line.strip()], i + 1
            out.append(f'<p>{inline(" ".join(para))}</p>')
            i = j
        return '\n'.join(out)

    def starts_block(self, lines, j):
        l = lines[j]
        return bool(FENCE.match(l) or HEADING.match(l) or HR.match(l) or RAW_BLOCK.match(l) or l.startswith('>')
                    or (l.startswith('|') and j + 1 < len(lines) and TABLE_SEP.match(lines[j + 1]))
                    or (LIST_ITEM.match(l) and not LIST_ITEM.match(l)[1])
                    or (j + 1 < len(lines) and lines[j + 1].startswith(': ')))

    def quote(self, lines):
        kind = None
        if lines and (m := re.match(r'^\[!([A-Z]+)\]\s*(.*)$', lines[0])) and m[1] in CALLOUTS:
            kind = m[1]
            lines = ([m[2]] if m[2] else []) + lines[1:]
        inner = self.blocks(lines)
        if not kind:
            return f'<blockquote>{inner}</blockquote>'
        return f'<div class="callout {kind.lower()}" role="note"><p class="callout-t">{CALLOUTS[kind]}</p>{inner}</div>'

    def table(self, head, sep, rows):
        cells = lambda l: [c.strip() for c in l.strip().strip('|').split('|')]
        aligns = ['right' if c.endswith(':') and not c.startswith(':') else 'center' if c.startswith(':') and c.endswith(':') else ''
                  for c in cells(sep)]
        style = lambda k: f' style="text-align:{aligns[k]}"' if k < len(aligns) and aligns[k] else ''
        th = ''.join(f'<th{style(k)}>{inline(c)}</th>' for k, c in enumerate(cells(head)))
        body = ''.join('<tr>' + ''.join(f'<td{style(k)}>{inline(c)}</td>' for k, c in enumerate(cells(r))) + '</tr>' for r in rows)
        return f'<div class="table"><table><thead><tr>{th}</tr></thead><tbody>{body}</tbody></table></div>'

    def list(self, lines, i, out):
        ordered = LIST_ITEM.match(lines[i])[2][0].isdigit()
        start = int(LIST_ITEM.match(lines[i])[2][:-1]) if ordered else 1
        items, loose = [], False
        while i < len(lines):
            m = LIST_ITEM.match(lines[i])
            if not m or m[1] or m[2][0].isdigit() != ordered:
                break
            width = len(m[2]) + len(m[3])
            body = [m[4]]
            j = i + 1
            while j < len(lines):
                l = lines[j]
                if not l.strip():
                    # a blank line continues the item only if indented content follows
                    k = j
                    while k < len(lines) and not lines[k].strip():
                        k += 1
                    if k < len(lines) and lines[k].startswith(' ' * width):
                        body += [''] * (k - j)
                        loose = loose or not LIST_ITEM.match(lines[k][width:] if lines[k].startswith(' ' * width) else '')
                        j = k
                        continue
                    break
                if l.startswith(' ' * width):
                    body.append(l[width:])
                elif LIST_ITEM.match(l) and not LIST_ITEM.match(l)[1]:
                    break
                elif not self.starts_block(lines, j) and not l.startswith(' '):
                    body.append(l)  # a lazy continuation of the paragraph
                else:
                    break
                j += 1
            items.append(body)
            i = j
            # a blank line between two items makes the list loose
            k = i
            while k < len(lines) and not lines[k].strip():
                k += 1
            if k > i and k < len(lines) and (m2 := LIST_ITEM.match(lines[k])) and not m2[1] and m2[2][0].isdigit() == ordered:
                loose = True
                i = k
        tag = 'ol' if ordered else 'ul'
        attrs = f' start="{start}"' if ordered and start != 1 else ''
        html_items = []
        for body in items:
            inner = self.blocks(body)
            if not loose:
                inner = re.sub(r'^<p>(.*?)</p>', r'\1', inner, count=1, flags=re.S)
            html_items.append(f'<li>{inner}</li>')
        out.append(f'<{tag}{attrs}>' + ''.join(html_items) + f'</{tag}>')
        return i

    def deflist(self, lines, i, out):
        parts = []
        while i + 1 < len(lines) and lines[i].strip() and lines[i + 1].startswith(': '):
            parts.append(f'<dt>{inline(lines[i].strip())}</dt>')
            i += 1
            while i < len(lines) and lines[i].startswith(': '):
                d = [lines[i][2:].strip()]
                i += 1
                while i < len(lines) and lines[i].startswith('  ') and lines[i].strip():
                    d.append(lines[i].strip())
                    i += 1
                parts.append(f'<dd>{inline(" ".join(d))}</dd>')
            while i < len(lines) and not lines[i].strip():
                i += 1
        out.append('<dl>' + ''.join(parts) + '</dl>')
        return i


def front_matter(text):
    """(dict, body) from a file that may start with ---\\nkey: value\\n---"""
    meta = {}
    if text.startswith('---\n'):
        head, _, text = text[4:].partition('\n---\n')
        for line in head.splitlines():
            if line.strip() and not line.startswith('#'):
                k, _, v = line.partition(':')
                meta[k.strip()] = v.strip()
    return meta, text


def render(markdown):
    """(html, headings) for a Markdown body; headings are (level, id, inner html)"""
    r = Renderer()
    body = r.blocks(markdown.replace('\r\n', '\n').replace('\t', '    ').split('\n'))
    return body, r.headings
