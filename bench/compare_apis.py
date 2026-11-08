"""The handle-based C interface (StoreModel) vs the database-like one (DbModel), same inventory, same process.

usage: compare_apis.py inventory_v2.json
"""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "qt", "build"))
import store_qt  # noqa: E402

from inventory_store import Inventory, NativeStore  # noqa: E402

inv = Inventory()
with open(sys.argv[1]) as f:
    for key, tree in json.load(f).items():
        if type(tree) is dict and "__spz_type__" in tree:
            inv.attach(key, NativeStore.from_dict(tree))
inv.search("x")  # builds the search indexes once, for both

store_qt.attach(inv)
print("connected:", store_qt.db_attach(inv))
for label, walk, search in (("handles (StoreModel)", store_qt.walk, store_qt.search),
                            ("database (DbModel)", store_qt.db_walk, store_qt.db_search)):
    walks = [walk() for _ in range(3)]
    best = min(w["ms"] for w in walks)
    s = min((search("mms1 fgm bgse") for _ in range(3)), key=lambda r: r["ms"])
    print(f"{label:22s} walk {walks[0]['nodes']} nodes: {best:6.1f} ms (bad parents {walks[0]['bad_parents']}) | "
          f"search {s['hits']} hits: {s['ms']:6.1f} ms | 1000 tooltips {s['tooltips_1000_ms']:5.1f} ms")
print("db model errors:", store_qt.db_process_events()["errors"])
