"""
Builds the website's generated pages from one template:

    guide/index.html    the program's user guide (docs/USER_GUIDE.md)
    privacy/index.html  what this website counts and why
    404.html            the page-not-found page
    sitemap.xml         every page, with the date it last changed

    python site/tools/build_pages.py

The guide is the same text the program shows under F1, so the two never
disagree. It uses a small part of Markdown: `#` topics, `##` sections,
paragraphs, `-` and numbered lists, **bold** and `code`.
"""
import datetime
import hashlib
import html
import pathlib
import re
import subprocess

SITE = pathlib.Path(__file__).resolve().parents[1]
REPOSITORY = SITE.parent
PUBLIC = SITE / "public"
ORIGIN = "https://quintess.cc"

TEMPLATE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<meta name="description" content="{description}">
{canonical}<meta name="theme-color" content="#0a0a0f">
<meta name="color-scheme" content="dark">
{robots}<link rel="icon" href="/favicon.ico" sizes="any">
<link rel="apple-touch-icon" href="/assets/img/apple-touch-icon.png">
<link rel="manifest" href="/site.webmanifest">
<meta property="og:type" content="website">
<meta property="og:site_name" content="Quintess">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{description}">
<meta property="og:image" content="https://quintess.cc/assets/img/og.png">
<meta name="twitter:card" content="summary_large_image">
<link rel="preload" href="/assets/fonts/archivo-wdth.woff2" as="font" type="font/woff2" crossorigin>
<link rel="stylesheet" href="/assets/site.css">
{head_extra}</head>
<body class="subpage">
<a class="skip" href="#main">Skip to content</a>
<div class="grid-lines" aria-hidden="true"></div>
<header class="top">
  <div class="top-in">
    <a class="brand" href="/" aria-label="Quintess home"><img src="/assets/img/mark.png" alt="" width="20" height="24"><span>quintess</span></a>
    <nav class="nav" aria-label="Main">
      <a href="/#features">Features</a>
      <a href="/#private">Privacy</a>
      <a href="/guide/"{guide_current}>Guide</a>
      <a href="https://github.com/bhanright/quintess" data-track="github">GitHub</a>
    </nav>
    <span class="readout" aria-hidden="true"></span>
    <a class="button button-small" href="/#download">Download</a>
  </div>
  <div class="progress" aria-hidden="true"><span></span></div>
</header>
<main id="main">
{body}
</main>
<footer class="foot">
  <div class="foot-in">
    <a class="brand" href="/"><img src="/assets/img/mark.png" alt="" width="18" height="22"><span>quintess</span></a>
    <nav aria-label="Footer">
      <a href="/guide/">User guide</a>
      <a href="/privacy/">Site privacy</a>
      <a href="https://github.com/bhanright/quintess/blob/main/LICENSE-MIT">License</a>
      <a href="https://github.com/bhanright/quintess">GitHub</a>
    </nav>
    <p class="mono faint keep-case">© 2026 William Hanright · MIT or Apache-2.0 · this site sets no cookies</p>
  </div>
</footer>
<script src="/assets/site.js" defer></script>
</body>
</html>
"""


def page(title: str, description: str, body: str, path: str | None, head_extra: str = "", guide: bool = False) -> str:
    return TEMPLATE.format(
        title=html.escape(title),
        description=html.escape(description),
        canonical=f'<link rel="canonical" href="{ORIGIN}{path}">\n' if path else "",
        robots="" if path else '<meta name="robots" content="noindex">\n',
        head_extra=head_extra,
        guide_current=' aria-current="page"' if guide else "",
        body=body,
    )


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def inline(text: str) -> str:
    """**bold** and `code` spans, with everything else escaped."""
    out = []
    for piece in re.split(r"(\*\*.+?\*\*|`[^`]+`)", text):
        if piece.startswith("**") and piece.endswith("**") and len(piece) > 4:
            out.append(f"<b>{html.escape(piece[2:-2])}</b>")
        elif piece.startswith("`") and piece.endswith("`") and len(piece) > 2:
            out.append(f"<code>{html.escape(piece[1:-1])}</code>")
        else:
            out.append(html.escape(piece))
    return "".join(out)


def guide_html(markdown: str) -> tuple[str, list[tuple[str, str]]]:
    """The guide's body and its list of topics (title, anchor)."""
    topics: list[tuple[str, str]] = []
    out: list[str] = []
    open_list: str | None = None

    def close_list() -> None:
        nonlocal open_list
        if open_list:
            out.append(f"</{open_list}>")
            open_list = None

    for raw in markdown.splitlines():
        line = raw.rstrip()
        numbered = re.match(r"^(\d+)\. (.*)$", line)
        if line.startswith("# "):
            close_list()
            if topics:
                out.append("</section>")
            title = line[2:].strip()
            anchor = slug(title)
            topics.append((title, anchor))
            out.append(f'<section class="doc-topic" id="{anchor}"><h2><a href="#{anchor}">{html.escape(title)}</a></h2>')
        elif line.startswith("## "):
            close_list()
            heading = line[3:].strip()
            out.append(f'<h3 id="{topics[-1][1]}-{slug(heading)}">{inline(heading)}</h3>')
        elif line.startswith("- "):
            if open_list != "ul":
                close_list()
                out.append("<ul>")
                open_list = "ul"
            out.append(f"<li>{inline(line[2:])}</li>")
        elif numbered:
            if open_list != "ol":
                close_list()
                out.append("<ol>")
                open_list = "ol"
            out.append(f"<li>{inline(numbered.group(2))}</li>")
        elif not line:
            close_list()
        else:
            close_list()
            out.append(f"<p>{inline(line)}</p>")
    close_list()
    if topics:
        out.append("</section>")
    return "\n".join(out), topics


def build_guide() -> str:
    markdown = (REPOSITORY / "docs" / "USER_GUIDE.md").read_text(encoding="utf-8")
    # On the web there is no F1 and the topic list has no search box above
    # it in the same place; say so plainly.
    markdown = markdown.replace(
        "Use the list on the left to jump to a topic, or type in the search box above it. Press **F1** at any time to come back here.",
        "Use the list of topics to jump around, or filter it with the search box. In the program, press **F1** to open this guide.",
    )
    body, topics = guide_html(markdown)
    toc = "\n".join(f'<li><a href="#{anchor}">{html.escape(title)}</a></li>' for title, anchor in topics)
    content = f"""<div class="doc">
  <aside class="doc-toc" aria-label="Topics">
    <p class="mono doc-toc-title">user guide · {len(topics)} topics</p>
    <input class="doc-search" type="search" placeholder="Filter topics" aria-label="Filter topics" data-doc-search>
    <ol>{toc}</ol>
  </aside>
  <article class="doc-body">
    <p class="kicker"><span class="dot"></span>the same guide as F1 in the program</p>
    <h1 class="display-2">Quintess user guide</h1>
{body}
  </article>
</div>"""
    breadcrumbs = """<script type="application/ld+json">
{"@context":"https://schema.org","@type":"TechArticle","headline":"Quintess user guide","about":{"@id":"https://quintess.cc/#app"},"author":{"@type":"Person","name":"William Hanright"},"url":"https://quintess.cc/guide/"}
</script>
"""
    return page(
        "Quintess user guide: editing, redaction, signatures, forms and more",
        "How to use every feature of Quintess, the free PDF editor for Windows: editing text, redaction, signatures, forms, OCR, QR codes, automation and the command line.",
        content,
        "/guide/",
        head_extra=breadcrumbs,
        guide=True,
    )


def build_privacy() -> str:
    body = """<article class="prose">
  <p class="kicker"><span class="dot"></span>site privacy</p>
  <h1 class="display-2">What this website counts.</h1>
  <p>This page is about the quintess.cc website. The Quintess program itself sends nothing anywhere; see the <a href="/guide/#privacy-and-updates">guide</a>.</p>
  <h2>No cookies, no trackers</h2>
  <p>The site sets no cookies, uses no third-party scripts, fonts or analytics services, and shows no ads. Fonts and images are served from quintess.cc itself.</p>
  <h2>A simple visit count</h2>
  <p>To know whether anyone finds the site, it counts page views and clicks on the download and GitHub links. For each, it keeps:</p>
  <ul>
    <li>the page, and the name of the site that linked to it (not the full address);</li>
    <li>the country, as reported by Cloudflare;</li>
    <li>whether the device is a desktop, phone or tablet, and the browser's name;</li>
    <li>a code that counts a visitor once per day. It is made from the date, your IP address, your browser's description and a secret, then scrambled one way. Because the date is part of it, the code changes every day, so visits on different days can't be connected, and it can't be turned back into your IP address.</li>
  </ul>
  <p>Your IP address itself is never stored. Counts older than 400 days are deleted automatically.</p>
  <h2>Opting out</h2>
  <p>If your browser sends Global Privacy Control or Do Not Track, nothing is counted. Blocking scripts also stops the count, and the site works fine without them.</p>
  <h2>Hosting</h2>
  <p>The site runs on Cloudflare, which handles the connection and may keep its own short-lived security logs. Downloads come from GitHub, under GitHub's own privacy terms.</p>
  <h2>Questions</h2>
  <p>Open an issue on <a href="https://github.com/bhanright/quintess/issues">GitHub</a>.</p>
</article>"""
    return page("Site privacy · Quintess", "What the quintess.cc website counts, and what it never collects.", body, "/privacy/")


def build_not_found() -> str:
    body = """<article class="prose not-found">
  <p class="kicker mono"><span class="dot"></span>error 404</p>
  <h1 class="display">Page not<br>found.</h1>
  <p>This page doesn't exist. It may have been redacted.</p>
  <p><a class="button" href="/">Back to the start</a></p>
</article>"""
    return page("Page not found · Quintess", "This page doesn't exist.", body, None)


def last_changed(*paths: pathlib.Path) -> str:
    """The date of the last commit touching any of these files, or today."""
    dates = []
    for path in paths:
        result = subprocess.run(["git", "log", "-1", "--format=%cs", "--", str(path)], cwd=REPOSITORY, capture_output=True, text=True)
        if result.stdout.strip():
            dates.append(result.stdout.strip())
    return max(dates) if dates else datetime.date.today().isoformat()


def build_sitemap() -> str:
    entries = [
        ("/", last_changed(PUBLIC / "index.html"), "1.0"),
        ("/guide/", last_changed(REPOSITORY / "docs" / "USER_GUIDE.md"), "0.8"),
        ("/privacy/", last_changed(SITE / "tools" / "build_pages.py"), "0.3"),
    ]
    urls = "\n".join(f"  <url><loc>{ORIGIN}{path}</loc><lastmod>{date}</lastmod><priority>{priority}</priority></url>" for path, date, priority in entries)
    return f'<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n{urls}\n</urlset>\n'


def write(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")


def stamp_assets() -> None:
    """Adds a short hash of site.css and site.js to every page's links to
    them, so browsers fetch a new copy as soon as either changes but can
    otherwise keep it cached."""
    for name in ("site.css", "site.js"):
        digest = hashlib.sha256((PUBLIC / "assets" / name).read_bytes()).hexdigest()[:10]
        for page_path in PUBLIC.rglob("*.html"):
            text = page_path.read_text(encoding="utf-8")
            stamped = re.sub(rf'/assets/{re.escape(name)}(\?v=[0-9a-f]+)?"', f'/assets/{name}?v={digest}"', text)
            if stamped != text:
                page_path.write_text(stamped, encoding="utf-8", newline="\n")


def main() -> None:
    write(PUBLIC / "guide" / "index.html", build_guide())
    write(PUBLIC / "privacy" / "index.html", build_privacy())
    write(PUBLIC / "404.html", build_not_found())
    write(PUBLIC / "sitemap.xml", build_sitemap())
    stamp_assets()
    print("built guide, privacy, 404 and sitemap; stamped asset links")


if __name__ == "__main__":
    main()
