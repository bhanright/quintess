"""
Generates candidate Quintess logos through OpenRouter, for choosing by eye.

    python tools/make_logo.py            # generate every candidate not yet made
    python tools/make_logo.py --force    # generate them all again

Candidates are written to logo_source/candidates/ (not committed). The chosen
one is turned into the program's icons by tools/make_icons.py.

The OpenRouter key is read from openrouter_key.txt in the folder above the
repository, outside it, so it is never committed.
"""
import argparse
import base64
import concurrent.futures
import json
import pathlib
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
KEY_FILE = ROOT.parent / "openrouter_key.txt"
API_URL = "https://openrouter.ai/api/v1/chat/completions"
OUTPUT_FOLDER = ROOT / "logo_source" / "candidates"

MODELS = {"gpt": "openai/gpt-5.4-image-2", "gemini": "google/gemini-3-pro-image"}

STYLE = (
    "A professional app icon for a desktop PDF editor called Quintess. Flat vector style, crisp geometric "
    "shapes, no gradients except one subtle one, no texture, no text, no letters other than any described, "
    "no drop shadow outside the tile. It must read clearly when shrunk to 16 pixels, so use at most three "
    "bold shapes. Colors: a near-black ink tile (#0B0B10), electric violet (#7C5CFF) as the main color, a "
    "small cyan (#22D3EE) accent, and white. A square rounded tile filling the whole image edge to edge, "
    "corner radius about 22 percent, centered, on a plain pure white background outside the tile."
)

CONCEPTS = {
    # The name comes from quintessence, the fifth element; the program edits pages.
    "q_page": "The mark is a bold geometric capital letter Q whose round body is a page of paper with a folded "
    "top-right corner; the Q's tail is a short cyan stroke like a pen nib crossing the lower right.",
    "fifth": "The mark is a white sheet of paper with a folded corner, and cut cleanly out of the sheet's middle is "
    "a five-pointed facet shape, a pentagon made of five violet triangular facets meeting at a center, like a gem, "
    "with one facet in cyan.",
    "q_monogram": "The mark is a thick, modern, perfectly circular capital Q drawn as a single violet ring with "
    "a squared-off cyan tail, with a small white document rectangle with a folded corner sitting inside the ring.",
}


def api_key() -> str:
    return KEY_FILE.read_text().strip()


def generate(prompt: str, model: str) -> bytes:
    body = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": [{"type": "text", "text": prompt}]}],
        "modalities": ["image", "text"],
        "image_config": {"aspect_ratio": "1:1"},
    }).encode()
    for attempt in range(3):
        request = urllib.request.Request(API_URL, data=body, headers={"Authorization": f"Bearer {api_key()}", "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                answer = json.loads(response.read())
            images = answer["choices"][0]["message"].get("images") or []
            if images:
                return base64.b64decode(images[0]["image_url"]["url"].split(",", 1)[1])
            print(f"  no image in the answer: {json.dumps(answer)[:300]}")
        except Exception as error:  # network errors and 5xx: wait and try again
            print(f"  request failed ({error}); retrying")
        time.sleep(5 * (attempt + 1))
    raise RuntimeError("OpenRouter gave no image")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--force", action="store_true")
    arguments = parser.parse_args()
    OUTPUT_FOLDER.mkdir(parents=True, exist_ok=True)
    jobs = []
    for concept, description in CONCEPTS.items():
        for short_name, model in MODELS.items():
            path = OUTPUT_FOLDER / f"{concept}-{short_name}.png"
            if arguments.force or not path.exists():
                jobs.append((path, f"{STYLE} {description}", model))
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        futures = {pool.submit(generate, prompt, model): path for path, prompt, model in jobs}
        for future in concurrent.futures.as_completed(futures):
            path = futures[future]
            try:
                path.write_bytes(future.result())
                print("wrote", path.name)
            except Exception as error:
                print("failed", path.name, error)


if __name__ == "__main__":
    main()
