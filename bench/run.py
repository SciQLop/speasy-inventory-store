"""Runs bench.py once per implementation and repetition, one process at a time; prints a markdown table.

usage: run.py inventory.json [repetitions]
Times: best run. Memory: median run.
"""
import json
import os
import statistics
import subprocess
import sys

IMPLS = ("tree", "columnar", "blob", "native")
ROWS = [  # (label, metric, format)
    ("build from parsed JSON dict", "build_dict_s", "{:.2f} s"),
    ("build from JSON text", "build_json_s", "{:.2f} s"),
    ("build through add() per node", "builder_s", "{:.2f} s"),
    ("memory: Python objects + native heap", "objects_mb", "{:.0f} MB"),
    ("memory: RSS of a process that only loads it", "rss_clean_mb", "{:.0f} MB"),
    ("JSON export", "to_json_s", "{:.2f} s"),
    ("legacy tree rebuild", "legacy_tree_s", "{:.2f} s"),
    ("legacy pickle (rebuild + protocols 3, 4, 5)", "legacy_pickle_s", "{:.2f} s"),
    ("legacy tree, transient memory", "legacy_transient_mb", "{:.0f} MB"),
    ("pickle size (protocol 3)", "pickle_mb", "{:.1f} MB"),
    ("uid lookup", "uid_lookup_us", "{:.2f} µs"),
    ("path lookup", "path_lookup_us", "{:.2f} µs"),
    ("full walk, children + meta dict per node", "walk_s", "{:.2f} s"),
    ("query: cda parameters in nT", "query_s", "{:.3f} s"),
]
SIZES = ("objects_mb", "rss_clean_mb", "legacy_transient_mb", "pickle_mb")


def last_json_line(script, impl, data):
    path = os.path.join(os.path.dirname(__file__), script)
    out = subprocess.run([sys.executable, path, impl, data], check=True, capture_output=True, text=True).stdout
    return json.loads(out.strip().splitlines()[-1])


def run(impl, data):
    return {**last_json_line("bench.py", impl, data), **last_json_line("clean_rss.py", impl, data)}


def summary(runs):
    keys = {k for r in runs for k, v in r.items() if isinstance(v, (int, float))}
    pick = lambda k, vals: statistics.median(vals) if k in SIZES else min(vals)  # noqa: E731
    return {k: pick(k, [r[k] for r in runs if r.get(k) is not None]) for k in keys
            if any(r.get(k) is not None for r in runs)}


def main(data, repetitions="3"):
    results = {impl: [] for impl in IMPLS}
    for _ in range(int(repetitions)):
        for impl in IMPLS:
            results[impl].append(run(impl, data))
            print(f"{impl}: done", file=sys.stderr, flush=True)
    best = {impl: summary(runs) for impl, runs in results.items()}
    print("| | Python tree (today) | Python columnar, interned str | Python columnar, one str blob | Rust store |\n|---|---|---|---|---|")
    for label, key, fmt in ROWS:
        cells = [fmt.format(best[i][key]) if key in best[i] else "—" for i in IMPLS]
        print(f"| {label} | " + " | ".join(cells) + " |")
    print(f"\nquery hits: {best['tree'].get('query_hits')}")
    with open(os.path.join(os.path.dirname(__file__), "..", "results.json"), "w") as f:
        json.dump(results, f, indent=1)


if __name__ == "__main__":
    main(*sys.argv[1:])
