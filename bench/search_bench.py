"""Search timings on the real corpus. usage: search_bench.py inventory.json  (RAYON_NUM_THREADS=1 for one core)"""
import json, os, sys, time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "tests"))
import bm25_reference
from inventory_store import NativeStore

FUZZY = ["mag", "mms1 fgm bgse", "provider:cda density", "provider:cda after:2020 b_gse", "magnetic field vector"]
BM25 = ["mms fgm b gse", "solar wind proton density", "electron pitch angle distribution"]


def best(f, n=3):
    times = []
    for _ in range(n):
        t = time.perf_counter(); r = f(); times.append(time.perf_counter() - t)
    return min(times), r


st = NativeStore.from_json(open(sys.argv[1]).read())
t_build, leaves = best(st.build_search, 1)
out = {"threads": os.environ.get("RAYON_NUM_THREADS", str(os.cpu_count())), "leaves": leaves, "build_search_s": t_build,
       "search_heap_mb": st.search_heap_bytes() / 2 ** 20}
for q in FUZZY:
    t, hits = best(lambda: st.search(q))
    out[f"fuzzy[{q}]"] = (t, len(hits))
for q in BM25:
    out[f"bm25[{q}]"] = (best(lambda: st.bm25(q))[0], len(st.bm25(q)))
if out["threads"] == "1":
    corpus = st.corpus()
    t, index = best(lambda: bm25_reference.build(corpus), 1)
    out["python_bm25_build_s"] = t
    for q in BM25:
        out[f"python_bm25[{q}]"] = best(lambda: bm25_reference.score(index, q))[0]
    json.dump([[p, m] for _, p, m in corpus], open(os.path.join(os.path.dirname(sys.argv[1]), "corpus.json"), "w"))
print(json.dumps(out, indent=1))
