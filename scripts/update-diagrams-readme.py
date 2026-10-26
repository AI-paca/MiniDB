"""Обновить только раздел иллюстраций README после экспорта drawio в PNG."""
from pathlib import Path
import html
import sys
from urllib.parse import quote
import xml.etree.ElementTree as ET

root = Path(__file__).resolve().parent.parent
start = '<!-- architecture:start -->'
end = '<!-- architecture:end -->'
readme = root / 'README.md'
text = readme.read_text()
entries = Path(sys.argv[1]).read_bytes().split(b'\0')
diagrams = {}
for source, png in zip(entries[:-1:2], entries[1::2]):
    diagrams.setdefault(source.decode(), []).append(png.decode())
lines = [start]
for index, (source, images) in enumerate(sorted(diagrams.items())):
    page = ET.parse(root / source).getroot().find('diagram')
    title = (page.get('name') if page is not None else None) or Path(source).stem
    title = html.escape(title, quote=False).replace('[', '&#91;').replace(']', '&#93;').replace('\n', ' ')
    rendered = []
    for png in sorted(images):
        if not (root / png).is_file():
            raise SystemExit(f'Нет результата экспорта: {png}')
        rendered.extend([f'![{title}]({quote(png, safe="/")})', ''])
    link = f'[Исходник drawio]({quote(source, safe="/")})'
    if index:
        lines.extend(['', f'<details><summary>{title}</summary>', '', *rendered, link, '', '</details>'])
    else:
        lines.extend(['', *rendered, link])
lines.extend(['', end])
block = '\n'.join(lines)
if text.count(start) != text.count(end) or text.count(start) > 1:
    raise SystemExit('Повреждены маркеры раздела архитектуры в README')
if start in text:
    before, after = text.split(start, 1)
    _, after = after.split(end, 1)
    text = before + block + after
else:
    text = text.rstrip() + '\n\n' + block + '\n'
readme.write_text(text)
