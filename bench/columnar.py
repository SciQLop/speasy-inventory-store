"""The same columnar store in plain Python: the baseline that tells what Rust adds over removing per-node objects.

Strings are interned (one object per distinct string), node fields are parallel lists, row ids and offsets are
compact arrays, metadata is one flat run of keys and values per node, children are CSR.
"""
import json
import sys
from array import array

SPZ_KEYS = ("__spz_type__", "__spz_name__", "__spz_provider__", "__spz_uid__")
_intern = sys.intern
NONE = 0xFFFFFFFF


def _is_node(value) -> bool:
    return type(value) is dict and "__spz_type__" in value


def _interned(value):
    return _intern(value) if type(value) is str else value


class PyStore:
    __slots__ = ("key", "name", "kind", "provider", "uid", "parent", "meta_start", "meta_key", "meta_val",
                 "child_start", "child", "by_uid")

    def __init__(self):
        self.key, self.name, self.kind, self.provider, self.uid = [], [], [], [], []
        self.parent, self.meta_start = array("I"), array("I")
        self.meta_key, self.meta_val = [], []
        self.child_start, self.child, self.by_uid = array("I"), array("I"), {}

    @staticmethod
    def from_dict(root: dict) -> "PyStore":
        store = PyStore()
        store._load(NONE, "", root)
        store.finish()
        return store

    @staticmethod
    def from_json(text: str) -> "PyStore":
        return PyStore.from_dict(json.loads(text))

    def _load(self, parent: int, key: str, node: dict):
        meta = {k: v for k, v in node.items() if k not in SPZ_KEYS and not _is_node(v)}
        me = self.add(parent, key, node["__spz_name__"], node["__spz_type__"], node["__spz_provider__"],
                      node["__spz_uid__"], meta)
        for k, v in node.items():
            if _is_node(v):
                self._load(me, k, v)

    def add(self, parent, key, name, kind, provider, uid, meta=None) -> int:
        """Builder API: one call per node, parents before children; call finish() at the end."""
        me = len(self.name)
        for column, value in zip((self.key, self.name, self.kind, self.provider, self.uid),
                                 (key, name, kind, provider, uid)):
            column.append(_intern(value))
        self.parent.append(NONE if parent is None else parent)
        self.meta_start.append(len(self.meta_key))
        for k, v in (meta or {}).items():
            self.meta_key.append(_intern(k))
            self.meta_val.append(_interned(v))
        return me

    def finish(self):
        n = len(self.name)
        self.meta_start.append(len(self.meta_key))
        start = array("I", bytes(4 * (n + 1)))
        for p in self.parent:
            if p != NONE:
                start[p + 1] += 1
        for i in range(n):
            start[i + 1] += start[i]
        nxt, child = array("I", start), array("I", bytes(4 * start[n]))
        for me, p in enumerate(self.parent):
            if p != NONE:
                child[nxt[p]] = me
                nxt[p] += 1
        self.child_start, self.child = start, child
        by_uid = {}
        for me in range(n - 1, -1, -1):
            by_uid.setdefault(self.provider[me], {})[self.uid[me]] = me
        self.by_uid = by_uid

    def __len__(self):
        return len(self.name)

    def children(self, i):
        return self.child[self.child_start[i]:self.child_start[i + 1]].tolist()

    def meta(self, i) -> dict:
        lo, hi = self.meta_start[i], self.meta_start[i + 1]
        return dict(zip(self.meta_key[lo:hi], self.meta_val[lo:hi]))

    def get(self, i, key):
        lo, hi = self.meta_start[i], self.meta_start[i + 1]
        for m in range(lo, hi):
            if self.meta_key[m] == key:
                return self.meta_val[m]
        return None

    def parent_of(self, i):
        p = self.parent[i]
        return None if p == NONE else p

    def find_uid(self, provider, uid):
        return self.by_uid.get(provider, {}).get(uid)

    def find_path(self, path):
        node = 0
        for key in path.split("/"):
            node = next((c for c in self.children(node) if self.key[c] == key), None)
            if node is None:
                return None
        return node

    def filter(self, provider=None, kind=None, contains=None):
        def keep(i):
            if provider is not None and self.provider[i] != provider:
                return False
            if kind is not None and self.kind[i] != kind:
                return False
            if contains is not None:
                value = self.get(i, contains[0])
                return type(value) is str and contains[1] in value
            return True
        return [i for i in range(len(self.name)) if keep(i)]

    def _to_dict(self, i):
        d = self.meta(i)
        d.update((self.key[c], self._to_dict(c)) for c in self.children(i))
        d.update(zip(SPZ_KEYS, (self.kind[i], self.name[i], self.provider[i], self.uid[i])))
        return d

    def to_json(self) -> str:
        return json.dumps(self._to_dict(0))

    def to_legacy(self, types: dict, default):
        """Rebuilds a tree of the old index classes ({type name: class}), for the legacy pickle."""
        cls = types.get(self.kind[0], default)
        return self._legacy(0, types, default, cls)

    def _legacy(self, i, types, default, cls):
        obj = cls.__new__(cls)
        d = obj.__dict__
        d.update(self.meta(i))
        for c in self.children(i):
            d[self.key[c]] = self._legacy(c, types, default, types.get(self.kind[c], default))
        d.update(zip(SPZ_KEYS, (self.kind[i], self.name[i], self.provider[i], self.uid[i])))
        return obj
