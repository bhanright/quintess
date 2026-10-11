"""
Builds and deploys quintess.cc, then tells search engines which pages changed.

    python site/tools/deploy.py            # build, deploy, notify
    python site/tools/deploy.py --no-ping  # build and deploy only

Steps: build the generated pages (tools/build_pages.py), deploy the Worker
and its files with Wrangler, then send the sitemap's pages to IndexNow,
which passes them to Bing, Yandex, Seznam, Naver and others. Google reads
the sitemap itself (it is listed in robots.txt).
"""
import argparse
import json
import pathlib
import re
import subprocess
import sys
import urllib.request

SITE = pathlib.Path(__file__).resolve().parents[1]
PUBLIC = SITE / "public"
HOST = "quintess.cc"
# IndexNow proves the site is ours by this key, served at /<key>.txt.
INDEXNOW_KEY = "2dc38a316ece30ddbc6f57a1278e2b07"


def run(command: list[str]) -> None:
    print("$", " ".join(command))
    subprocess.run(command, cwd=SITE, check=True, shell=sys.platform == "win32")


def ping_indexnow() -> None:
    sitemap = (PUBLIC / "sitemap.xml").read_text(encoding="utf-8")
    urls = re.findall(r"<loc>(.*?)</loc>", sitemap)
    body = json.dumps({"host": HOST, "key": INDEXNOW_KEY, "keyLocation": f"https://{HOST}/{INDEXNOW_KEY}.txt", "urlList": urls}).encode()
    request = urllib.request.Request("https://api.indexnow.org/indexnow", data=body, headers={"Content-Type": "application/json; charset=utf-8"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            print(f"IndexNow accepted {len(urls)} pages (HTTP {response.status})")
    except urllib.error.HTTPError as error:
        # 202 arrives as success above; 422 or 403 usually means the key file isn't live yet.
        print(f"IndexNow answered HTTP {error.code}: {error.read()[:200]!r}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--no-ping", action="store_true")
    arguments = parser.parse_args()
    (PUBLIC / f"{INDEXNOW_KEY}.txt").write_text(INDEXNOW_KEY, encoding="utf-8")
    run([sys.executable, "tools/build_pages.py"])
    run(["npx", "wrangler", "deploy"])
    if not arguments.no_ping:
        ping_indexnow()


if __name__ == "__main__":
    main()
