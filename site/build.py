#!/usr/bin/env python3
"""Build the Locus landing page into a single self-contained HTML file.

Reads page.html (the editable template), inlines the webfonts as base64
data URIs, wraps the result in a full HTML document and writes index.html.

The output makes no external requests at all — no font CDN, no analytics,
no third-party anything — which is the same promise the tool itself makes.

    python3 site/build.py
"""

import base64
import hashlib
import re
import sys
import urllib.request
from pathlib import Path

HERE = Path(__file__).parent

FONT_CSS_URL = (
    "https://fonts.googleapis.com/css2"
    "?family=Source+Serif+4:ital,opsz,wght@0,8..60,400;0,8..60,600;0,8..60,700;1,8..60,400"
    "&family=IBM+Plex+Mono:wght@400;500;600"
    "&display=swap"
)

# Google serves different @font-face blocks per browser; a modern UA gets woff2.
UA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36"


def _get(url: str, ua: bool = False) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": UA} if ua else {})
    return urllib.request.urlopen(req, timeout=60).read()


def _field(block: str, name: str):
    m = re.search(name + r":\s*([^;]+);", block)
    return m.group(1).strip() if m else None


def build_font_css() -> str:
    """Download the latin subsets and emit @font-face rules with inline payloads."""
    css = _get(FONT_CSS_URL, ua=True).decode()

    # Group by (family, style, payload hash). Variable fonts return one identical
    # payload for several weights — collapsing them saves ~240KB of base64.
    faces: dict = {}
    for block in re.findall(r"@font-face\s*\{(.*?)\}", css, re.S):
        if not (_field(block, "unicode-range") or "").startswith("U+0000-00FF"):
            continue  # latin basic only; the page has no other scripts
        family = _field(block, "font-family").strip("\"'")
        style = _field(block, "font-style") or "normal"
        weight = int(_field(block, "font-weight") or 400)
        url = re.search(r"url\((https://[^)]+)\)", _field(block, "src")).group(1)

        data = _get(url)
        key = (family, style, hashlib.sha1(data).hexdigest())
        faces.setdefault(key, {"data": data, "weights": []})["weights"].append(weight)

    rules = []
    for (family, style, _hash), face in faces.items():
        weights = sorted(face["weights"])
        decl = str(weights[0]) if len(weights) == 1 else f"{weights[0]} {weights[-1]}"
        payload = base64.b64encode(face["data"]).decode()
        print(f"  {family:16} {style:7} {decl:10} {len(face['data']):>7} B", file=sys.stderr)
        rules.append(
            '@font-face{font-family:"%s";font-style:%s;font-weight:%s;font-display:swap;'
            'src:url(data:font/woff2;base64,%s) format("woff2");}' % (family, style, decl, payload)
        )
    return "\n".join(rules)


DOCUMENT = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="description" content="Locus is a local-first index for AI agents. It answers structural questions with precise pointers — file, symbol, byte range — instead of dumping whole files into the context window.">
<meta name="color-scheme" content="light dark">
{head}
</head>
<body>
{body}
</body>
</html>
"""


def main() -> int:
    template = (HERE / "page.html").read_text()
    if "/*__FONTS__*/" not in template:
        print("error: page.html is missing the /*__FONTS__*/ placeholder", file=sys.stderr)
        return 1

    print("fetching fonts…", file=sys.stderr)
    page = template.replace("/*__FONTS__*/", build_font_css())

    # <title> and <style> belong in <head>; everything else is the body.
    head = "\n".join(
        m.group(0)
        for m in re.finditer(r"<title>.*?</title>|<style>.*?</style>", page, re.S)
    )
    body = re.sub(r"<title>.*?</title>|<style>.*?</style>", "", page, flags=re.S).strip()

    out = HERE / "index.html"
    out.write_text(DOCUMENT.format(head=head, body=body))
    print(f"wrote {out.relative_to(HERE.parent)} — {out.stat().st_size / 1024:.0f} KB, zero external requests", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
