"""Convert captured ANSI cells to an offline JSON reference for the design study."""
from pathlib import Path
import json
import re

ROOT = Path(__file__).parent
ANSI = ['#151719', '#ee94a0', '#a1d1b0', '#f3c575', '#86b5ff', '#c0a9e8', '#8fcad4', '#eceff4',
        '#7d8793', '#ee94a0', '#a1d1b0', '#f3c575', '#86b5ff', '#c0a9e8', '#8fcad4', '#ffffff']


def cells(line):
    fg, bg, bold = '#ebebeb', '#141414', False
    result = []
    for part in re.split(r'(\x1b\[[0-9;]*m)', line):
        if part.startswith('\x1b['):
            codes = [int(n or 0) for n in part[2:-1].split(';')]
            i = 0
            while i < len(codes):
                c = codes[i]
                if c == 0:
                    fg, bg, bold = '#ebebeb', '#141414', False
                elif c in (1, 22):
                    bold = c == 1
                elif c == 39:
                    fg = '#ebebeb'
                elif c == 49:
                    bg = '#141414'
                elif 30 <= c <= 37 or 90 <= c <= 97:
                    fg = ANSI[c - 30 if c < 90 else c - 90 + 8]
                elif 40 <= c <= 47 or 100 <= c <= 107:
                    bg = ANSI[c - 40 if c < 100 else c - 100 + 8]
                elif c in (38, 48) and i + 1 < len(codes):
                    if codes[i + 1] == 2:
                        color = '#%02x%02x%02x' % tuple(codes[i + 2:i + 5])
                        i += 4
                    elif codes[i + 1] == 5:
                        n = codes[i + 2]
                        if n < 16:
                            color = ANSI[n]
                        elif n >= 232:
                            v = 8 + 10 * (n - 232)
                            color = '#%02x%02x%02x' % (v, v, v)
                        else:
                            n -= 16
                            cube = [0, 95, 135, 175, 215, 255]
                            color = '#%02x%02x%02x' % (cube[n // 36], cube[n // 6 % 6], cube[n % 6])
                        i += 2
                    else:
                        i += 1
                        continue
                    if c == 38:
                        fg = color
                    else:
                        bg = color
                i += 1
        else:
            for char in part:
                result.append([char, fg, bg, bold])
    return result


captures = {p.stem: [cells(line) for line in p.read_text().splitlines()]
            for p in (ROOT / 'evidence').glob('*.ansi')}
out = 'window.FORGE_REFERENCES = ' + json.dumps(captures, ensure_ascii=False, separators=(',', ':')) + ';\n'
(ROOT / 'references.js').write_text(out)
print(f'Converted {len(captures)} actual terminal captures.')
