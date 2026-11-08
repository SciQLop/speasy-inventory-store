"""One implementation, every metric, in this process; prints one JSON line. Run through run.py.

usage: bench.py {tree,columnar,native} inventory.json
"""
import ctypes
import gc
import json
import os
import pickle
import random
import sys
import time
import tracemalloc
from functools import reduce

from speasy.core.inventory.indexes import SpeasyIndex, __INDEXES_TYPES__, from_dict, to_json

from blob import BlobStore
from columnar import PyStore
from speasy_inventory_store import NativeStore

_libc = ctypes.CDLL("libc.so.6")
PICKLE_PROTOCOLS = (3, 4, 5)
QUERY = dict(provider="cda", kind="ParameterIndex", contains=("UNITS", "nT"))


def rss_mb() -> float:
    with open("/proc/self/statm") as f:
        return int(f.read().split()[1]) * os.sysconf("SC_PAGE_SIZE") / 2 ** 20


def settle():
    gc.collect()
    _libc.malloc_trim(0)


def timed(f, *args):
    t = time.perf_counter()
    result = f(*args)
    return result, time.perf_counter() - t


def is_node(v):
    return type(v) is dict and "__spz_type__" in v


def samples(root: dict, n=10_000):
    """(provider, uid) and attribute paths of n random parameters."""
    found = []

    def walk(node, path):
        for k, v in node.items():
            if is_node(v):
                if v["__spz_type__"] == "ParameterIndex":
                    found.append(((v["__spz_provider__"], v["__spz_uid__"]), "/".join(path + [k])))
                walk(v, path + [k])
    walk(root, [])
    picked = random.Random(42).sample(found, n)
    return [uid for uid, _ in picked], [path for _, path in picked]


class Tree:
    """speasy today: indexes.from_dict, plus a flat (provider, uid) dict like speasy's flat inventories."""

    def __init__(self, root):
        self.root, self.by_uid = root, {}
        self._index(root)

    def _index(self, node):
        for v in node.__dict__.values():
            if isinstance(v, SpeasyIndex):
                self.by_uid.setdefault((v.spz_provider(), v.spz_uid()), v)
                self._index(v)

    @staticmethod
    def from_dict(d):
        return Tree(from_dict(d, version=2))

    @staticmethod
    def from_json(text):
        return Tree.from_dict(json.loads(text))

    def to_json(self):
        return to_json(self.root, sort_keys=False, version=2)

    def to_legacy(self, types, default):
        return self.root

    def find_uid(self, provider, uid):
        return self.by_uid.get((provider, uid))

    def find_path(self, path):
        return reduce(lambda node, key: node.__dict__.get(key) if node is not None else None, path.split("/"),
                      self.root)

    def walk(self):
        def visit(node):
            meta, count = {}, 1
            for k, v in node.__dict__.items():
                if isinstance(v, SpeasyIndex):
                    count += visit(v)
                else:
                    meta[k] = v
            return count
        return visit(self.root)

    def filter(self, provider, kind, contains):
        key, text = contains

        def visit(node, out):
            d = node.__dict__
            if (d["__spz_provider__"] == provider and d["__spz_type__"] == kind
                    and type(d.get(key)) is str and text in d[key]):
                out.append(node)
            for v in d.values():
                if isinstance(v, SpeasyIndex):
                    visit(v, out)
            return out
        return visit(self.root, [])


def store_walk(store):
    def visit(i):
        store.meta(i)
        return 1 + sum(visit(c) for c in store.children(i))
    return visit(0)


def builder(cls, root):
    store = cls()

    def walk(parent, key, node):
        meta = {k: v for k, v in node.items() if not k.startswith("__spz_") and not is_node(v)}
        me = store.add(parent, key, node["__spz_name__"], node["__spz_type__"], node["__spz_provider__"],
                       node["__spz_uid__"], meta)
        for k, v in node.items():
            if is_node(v):
                walk(me, k, v)
    walk(None, "", root)
    store.finish()
    return store


IMPLS = {"tree": Tree, "columnar": PyStore, "blob": BlobStore, "native": NativeStore}


def python_objects_mb(f, *args) -> float:
    """Size of the Python objects f builds: RSS undercounts it, the process reuses memory freed earlier."""
    tracemalloc.start()
    result = f(*args)
    size = tracemalloc.get_traced_memory()[0] / 2 ** 20
    tracemalloc.stop()
    del result
    settle()
    return size


def legacy_pickles(impl):
    types = dict(__INDEXES_TYPES__)
    transient = python_objects_mb(impl.to_legacy, types, SpeasyIndex) if not isinstance(impl, Tree) else 0.0
    tree, t_tree = timed(impl.to_legacy, types, SpeasyIndex)
    blobs, t_pickle = timed(lambda: [pickle.dumps(tree, protocol=p) for p in PICKLE_PROTOCOLS])
    del tree
    settle()
    return dict(legacy_tree_s=t_tree, legacy_pickle_s=t_tree + t_pickle, legacy_transient_mb=transient,
                pickle_mb=len(blobs[0]) / 2 ** 20)


def lookups(impl, uids, paths):
    _, t_uid = timed(lambda: [impl.find_uid(*u) for u in uids])
    _, t_path = timed(lambda: [impl.find_path(p) for p in paths])
    assert all(impl.find_uid(*u) is not None for u in uids[:100])
    return dict(uid_lookup_us=t_uid / len(uids) * 1e6, path_lookup_us=t_path / len(paths) * 1e6)


def main(name, path):
    cls = IMPLS[name]
    with open(path) as f:
        text = f.read()
    uids, paths = samples(json.loads(text))
    settle()
    rss0 = rss_mb()
    d = json.loads(text)
    impl, t_dict = timed(cls.from_dict, d)
    del d
    settle()
    out = dict(impl=name, build_dict_s=t_dict, rss_mb=rss_mb() - rss0)
    if name != "tree":
        out["builder_s"] = timed(builder, cls, json.loads(text))[1]
    settle()
    native_heap = impl.heap_bytes() / 2 ** 20 if name == "native" else 0.0
    out["objects_mb"] = python_objects_mb(lambda: cls.from_dict(json.loads(text))) + native_heap
    out["build_json_s"] = timed(cls.from_json, text)[1]
    settle()
    out["to_json_s"] = timed(impl.to_json)[1]
    out.update(legacy_pickles(impl))
    out.update(lookups(impl, uids, paths))
    out["walk_s"] = timed(impl.walk if name == "tree" else store_walk, *([] if name == "tree" else [impl]))[1]
    hits, out["query_s"] = timed(lambda: impl.filter(**QUERY))
    out["query_hits"] = len(hits)
    print(json.dumps(out))


if __name__ == "__main__":
    main(*sys.argv[1:])
