from pathlib import Path
from html.parser import HTMLParser
import xml.etree.ElementTree as ET
import re

ROOT = Path(__file__).parent
class PrototypeParser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.scenes = []
        self.buttons = []
        self.svgs = 0
    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if 'data-scene' in attrs:
            self.scenes.append((attrs['data-scene'], 'hidden' in attrs))
        if 'data-view' in attrs:
            self.buttons.append((attrs['data-view'], attrs['aria-pressed']))
        if tag == 'svg':
            self.svgs += 1

html = (ROOT / 'index.html').read_text()
p = PrototypeParser()
p.feed(html)
views = {'review', 'sessions', 'github', 'ci', 'issues'}
assert {v for v, _ in p.scenes} == views
assert {v for v, _ in p.buttons} == views
assert p.svgs == 5
assert [v for v, hidden in p.scenes if not hidden] == ['review']
assert [v for v, pressed in p.buttons if pressed == 'true'] == ['review']
for svg in re.findall(r'<svg\b.*?</svg>', html, re.S):
    root = ET.fromstring(svg)
    assert root.attrib['viewBox'] == '0 0 1680 980'
    assert root.attrib['aria-labelledby']
    ns = {'s': 'http://www.w3.org/2000/svg'}
    assert root.find('s:title', ns) is not None
    for text in root.findall('.//s:text', ns):
        assert 0 <= float(text.attrib['x']) <= 1680
        assert 0 <= float(text.attrib['y']) <= 980
assert (ROOT / 'review.svg').read_text() == re.findall(r'<svg\b.*?</svg>', html, re.S)[0]
assert 'el.hidden = el.dataset.scene !== view' in html
assert not re.search(r'<script[^>]+src=|<link[^>]+href=', html)
for number in ('818', '807', '806'):
    assert f'data-issue="{number}"' in html
assert 'issue-search' in html and 'issue-preview' in html
assert 'No description provided.' in html
assert 'Simulation only:' in html
assert 'not a live connection' in html
assert '.issue-browser{display:contents}' in html
assert '.issue-scene>svg{opacity:1}' in html
assert 'issue-overlay' not in html
assert 'details in the workspace, conversation unchanged' in html
print('PASS: five scenes, navigation wiring, SVG XML, issue snapshot and simulated handoff')
