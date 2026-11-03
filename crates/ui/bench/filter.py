"""Keystroke-to-render time of a list filter over synthetic Entries, in headless Chromium.

Not a CI gate. Needs cargo-leptos, the wasm32-unknown-unknown target and Playwright for Python. Run it from the
repository's root:

    python3 -m venv .venv && .venv/bin/pip install playwright && .venv/bin/playwright install chromium
    .venv/bin/python crates/ui/bench/filter.py [--entries 100000] [--rounds 5] [--list]

The `/bench` screen exists only with the workbench's `bench` feature, which no release or image has. So the script
builds the workbench with it into target/bench/site, runs the Service on a loopback port with a scratch store and
`[auth] dev_identity` under target/bench, measures, and stops the Service. With `--url`, it measures a Service already
serving a `bench` build instead. The screen renders the Audit screen's Entries panel over deterministic synthetic
Entries, or with `--list` the same Entries in the shared list every list screen uses (`parts::list`); it reads nothing
from the Service. Each sample starts at the keydown's own timestamp and ends after the first frame painted once the
shown count reflects the new text and every rendered row matches it. The Entries panel's rows must also be new; the
shared list keeps a row's element while the row stays shown, so its rows need only match.
"""

import argparse
import os
import socket
import statistics
import subprocess
import time
import urllib.request
from contextlib import contextmanager
from pathlib import Path

from playwright.sync_api import sync_playwright

# Typed one key at a time, then erased one key at a time; each key is one sample.
QUERIES = ["river raid", "pit", "usa", "00042", "zzz"]

SETUP = """
() => {
  window.__samples = [];
  // Rows rendered before the keystroke are marked, so a sample can't end on rows that merely still match.
  document.querySelector('.filter-text').addEventListener('keydown', (e) => {
    window.__down = e.timeStamp;
    window.__before = document.querySelector('p.shown').dataset.shown;
    document.querySelectorAll('tbody tr[id]').forEach((r) => { r.dataset.stale = ''; });
  }, { capture: true });
}
"""

# The rendered rows of the Entries panel's table and of the shared list.
TABLE_ROWS = "tbody tr[id]"
LIST_ROWS = ".list-body [role=row][id]"

# Resolves with the keystroke-to-paint time once the list shows `query`'s result.
SETTLED = """
([query, list]) => new Promise((resolve) => {
  const q = query.toLowerCase();
  const done = () => {
    const shown = document.querySelector('p.shown');
    if (shown.dataset.query !== query) return false;
    // One key more or less narrows or widens the result, so the same count is the same Entries: nothing to redraw.
    if (shown.dataset.shown === window.__before) return true;
    let rows;
    if (list) {
      rows = [...document.querySelectorAll('.list-body [role=row][id]')];
    } else {
      const body = document.querySelector('#panel-entries tbody, tbody');
      if (body.querySelector('tr.loading, tr[data-stale]')) return false;
      rows = [...body.querySelectorAll('tr[id]')];
    }
    if (Number(shown.dataset.shown) > 0 && rows.length === 0) return false;
    return rows.every((r) => r.textContent.toLowerCase().includes(q));
  };
  const poll = () => {
    if (!done()) { requestAnimationFrame(poll); return; }
    // The frame after the one that rendered the result has been painted.
    requestAnimationFrame(() => setTimeout(() => resolve(performance.now() - window.__down), 0));
  };
  poll();
})
"""


def percentile(samples, p):
    ordered = sorted(samples)
    return ordered[min(len(ordered) - 1, round(p / 100 * (len(ordered) - 1)))]


@contextmanager
def bench_service():
    """Builds the workbench with the `bench` feature and serves it; yields the Service's URL."""
    work = Path("target/bench").resolve()
    site = work / "site"
    subprocess.run(
        ["cargo", "leptos", "build", "--release", "--lib-features", "bench"],
        check=True,
        env={**os.environ, "LEPTOS_SITE_ROOT": str(site)},
    )
    (work / "roots").mkdir(parents=True, exist_ok=True)
    config = work / "title-select.toml"
    config.write_text(f'[roots.bench]\npath = "{work / "roots"}"\n\n[auth]\ndev_identity = "Benchmark"\n')
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    service = subprocess.Popen(
        ["target/release/title-select", "--store", str(work / "store"), "--config", str(config),
         "serve", "--listen", f"127.0.0.1:{port}", "--ui", str(site)]
    )
    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(f"{url}/api/v1/health")
                break
            except OSError:
                time.sleep(0.1)
        yield url
    finally:
        service.terminate()
        service.wait()


@contextmanager
def given(url):
    yield url


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--url", help="a Service already serving a `bench` build of the workbench")
    parser.add_argument("--entries", type=int, default=100_000)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--list", action="store_true", help="measure the shared list rather than the Entries panel")
    args = parser.parse_args()

    with (given(args.url) if args.url else bench_service()) as url:
        samples, version = measure(url, args)
    what = "the shared list" if args.list else "the Entries panel"
    print(f"{len(samples)} keystrokes after one warm-up round over {args.entries} Entries in {what}; {version}")
    print(f"p50 {statistics.median(samples):.1f} ms, p95 {percentile(samples, 95):.1f} ms, max {max(samples):.1f} ms")


def measure(url, args):
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport={"width": 1440, "height": 1000})
        page.goto(f"{url}/bench?entries={args.entries}" + ("&list=1" if args.list else ""))
        page.wait_for_selector(LIST_ROWS if args.list else TABLE_ROWS, timeout=120_000)
        print(f"{page.text_content('p.shown')} loaded")
        version = browser.version
        page.evaluate(SETUP)
        field = page.locator(".filter-text")
        field.focus()
        samples = []
        for round_ in range(args.rounds):
            for query in QUERIES:
                for i in range(1, len(query) + 1):
                    page.keyboard.press(query[i - 1])
                    sample = page.evaluate(SETTLED, [query[:i], args.list])
                    if round_ > 0:
                        samples.append(sample)
                for i in range(len(query) - 1, -1, -1):
                    page.keyboard.press("Backspace")
                    sample = page.evaluate(SETTLED, [query[:i], args.list])
                    if round_ > 0:
                        samples.append(sample)
        browser.close()
    return samples, version


if __name__ == "__main__":
    main()
