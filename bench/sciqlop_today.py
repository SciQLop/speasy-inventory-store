"""What SciQLop's product tree and search cost today, replaying its startup path (run with SciQLop's venv, Qt offscreen):
1. speasy's providers (init_providers, as SpeasyPlugin.__init__ does);
2. the product tree copy (build_product_tree + ProductsModel.add_node);
3. a search through a flat and a tree filter model (ProductsView's), until settled;
4. the smart search corpus snapshot and BM25F build (in a subprocess in SciQLop; its result lives in the main process).
Time and RSS growth per step, in one process (steps only allocate, so deltas add up).

usage: QT_QPA_PLATFORM=offscreen <SciQLop venv>/bin/python sciqlop_today.py
"""
import gc
import os
import pickle
import sys
import time

QUERY = "mms1 fgm bgse"


def rss_mb() -> float:
    with open("/proc/self/statm") as f:
        return int(f.read().split()[1]) * os.sysconf("SC_PAGE_SIZE") / 2 ** 20


def step(label, f):
    gc.collect()
    r, t = rss_mb(), time.perf_counter()
    result = f()
    gc.collect()
    print(f"{label:58s} {time.perf_counter() - t:7.2f} s  {rss_mb() - r:+8.0f} MB RSS")
    return result


from PySide6.QtCore import QCoreApplication  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402

app = QApplication(sys.argv)
import SciQLopPlots as sqp  # noqa: E402
from SciQLopPlots import (ProductsFlatFilterModel, ProductsModel, ProductsModelNode,  # noqa: E402
                          ProductsTreeFilterModel, QueryParser)

print(f"baseline after imports: {rss_mb():.0f} MB RSS")
def start_speasy():
    # speasy 1.8 loads its inventories on import, later versions on first use: count both
    from speasy.core.requests_scheduling.request_dispatch import init_providers
    init_providers()


step("1. import speasy + init_providers() (inventories from the proxy)", start_speasy)
sys.path.insert(0, os.path.expanduser("~/Documents/prog/SciQLop"))
from SciQLop.plugins.speasy_provider.speasy_provider import build_product_tree  # noqa: E402


def product_tree():
    root = ProductsModelNode("speasy", icon="speasy")
    build_product_tree(root, provider="Speasy")
    ProductsModel.instance().add_node([], root)
    QCoreApplication.processEvents()
    return root


step("2. SciQLop's product tree copy (build_product_tree)", product_tree)


def count_leaves(index=None, model=ProductsModel.instance()):
    from PySide6.QtCore import QModelIndex
    index = index or QModelIndex()
    n = model.rowCount(index)
    return (1 if n == 0 and index.isValid() else 0) + sum(count_leaves(model.index(r, 0, index)) for r in range(n))


print(f"   product model leaves: {count_leaves()}")


def settle(model):
    """The flat filter model scores in batches on a timer: run the event loop until its row count is stable."""
    last, stable = -1, 0
    while stable < 20:
        QCoreApplication.processEvents()
        time.sleep(0.005)
        n = model.rowCount()
        stable, last = (stable + 1, n) if n == last else (0, n)
    return last


flat = step("3a. flat filter model created", lambda: ProductsFlatFilterModel(ProductsModel.instance()))
hits = step(f"3b. flat filter: query {QUERY!r} until settled",
            lambda: (flat.set_query(QueryParser.parse(QUERY)), settle(flat))[1])
print(f"   flat filter hits: {hits}")


def tree_filter():
    m = ProductsTreeFilterModel()
    m.setSourceModel(ProductsModel.instance())
    return m


tree = step("3c. tree filter model created", tree_filter)
step(f"3d. tree filter: query {QUERY!r} until settled", lambda: (tree.set_query(QueryParser.parse(QUERY)), settle(tree)))

from SciQLop.components.smart_search import bm25_index  # noqa: E402
from SciQLop.components.smart_search.domain import NodeSnapshot  # noqa: E402

corpus_model = ProductsFlatFilterModel(ProductsModel.instance())
snapshot = step("4a. smart search corpus snapshot (main process)",
                lambda: [NodeSnapshot(k, f"{k} {v}") for k, v in corpus_model.corpus_snapshot().items()])
print(f"   corpus entries: {len(snapshot)}, text {sum(len(s.raw_text) for s in snapshot) / 2**20:.0f} MB of characters")
index = step("4b. BM25F build (SciQLop runs it in a subprocess)", lambda: bm25_index.build(snapshot))
# pickled and unpickled here only to replay SciQLop's subprocess -> main process transfer of an object built just above
blob = step("4c. pickling the index (subprocess -> main process)", lambda: pickle.dumps(index))
print(f"   pickled index: {len(blob) / 2**20:.0f} MB")
del index
index = step("4d. unpickled index, as the main process keeps it", lambda: pickle.loads(blob))
t = time.perf_counter()
bm25_index.score(index, "mms fgm b gse")
print(f"   BM25F query: {(time.perf_counter() - t) * 1000:.0f} ms")
for dims, name in ((256, "potion-base-8M"), (512, "potion-base-32M")):
    print(f"   embedding matrix if enabled ({name}, {dims} float32 per entry): {len(snapshot) * dims * 4 / 2**20:.0f} MB")
print(f"total: {rss_mb():.0f} MB RSS")
