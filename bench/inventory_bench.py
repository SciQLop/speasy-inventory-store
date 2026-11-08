"""One store holding everything vs an inventory of per-provider segments, on the same data: build, memory, search,
and the Qt model's walk and search. One mode per process.

usage: inventory_bench.py {single,segmented} inventory_v2.json
"""
import ctypes
import gc
import json
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "qt", "build"))
from bench import rss_mb  # noqa: E402

from inventory_store import Inventory, NativeStore  # noqa: E402

FUZZY = ["mag", "mms1 fgm bgse", "provider:cda density", "provider:cda after:2020 b_gse", "magnetic field vector"]
BM25 = ["mms fgm b gse", "solar wind proton density", "electron pitch angle distribution"]


def best(f, n=3):
    times = []
    for _ in range(n):
        t = time.perf_counter()
        f()
        times.append(time.perf_counter() - t)
    return min(times)


def build(mode, text):
    if mode == "single":
        s = NativeStore.from_json(text)
        s.build_search()
        return s
    inv = Inventory()
    for key, tree in json.loads(text).items():
        if type(tree) is dict and "__spz_type__" in tree:
            inv.attach(key, NativeStore.from_json(json.dumps(tree)))
    return inv


def main(mode, path):
    text = open(path).read()
    gc.collect()
    ctypes.CDLL("libc.so.6").malloc_trim(0)
    before = rss_mb()
    t = time.perf_counter()
    target = build(mode, text)
    out = {"mode": mode, "build_s": time.perf_counter() - t}
    t = time.perf_counter()
    target.search("x")  # the segmented inventory builds its per-segment indexes here
    out["first_search_s"] = time.perf_counter() - t
    gc.collect()
    ctypes.CDLL("libc.so.6").malloc_trim(0)
    out["rss_mb"] = rss_mb() - before
    for q in FUZZY:
        out[f"fuzzy[{q}]"] = best(lambda: target.search(q))
    for q in BM25:
        out[f"bm25[{q}]"] = best(lambda: target.bm25(q))
    if mode == "segmented":
        import store_qt
        store_qt.attach(target)
        out["qt_walk"] = store_qt.walk()
        out["qt_search"] = store_qt.search("mms1 fgm bgse")
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main(*sys.argv[1:])
