"""Columnar store in plain Python with no small objects left once built: every distinct string lives in one big str,
named by its offset. Built from a parsed dict, the per-node garbage is all freed, so Python can return its memory;
the interned-string columnar store keeps its strings scattered over the arenas the parse used (fragmentation).
"""
import json
from array import array

from .columnar import NONE, SPZ_KEYS, _is_node

OTHER = 1 << 31


class BlobStore:
    __slots__ = ("blob", "off", "cols", "parent", "meta_start", "meta_key", "meta_val", "others", "child_start",
                 "child", "by_uid", "_ids", "_parts", "_size")

    def __init__(self):
        self.cols = [array("I") for _ in range(5)]  # key, name, kind, provider, uid
        self.parent, self.meta_start, self.meta_key, self.meta_val = (array("I") for _ in range(4))
        self.others, self._ids, self._parts, self._size = [], {}, [], 0
        self.off, self.blob = array("I", [0]), ""

    def _id(self, s: str) -> int:
        i = self._ids.get(s)
        if i is None:
            i = self._ids[s] = len(self._parts)
            self._parts.append(s)
            self._size += len(s)
            self.off.append(self._size)
        return i

    def s(self, i: int) -> str:
        return self.blob[self.off[i]:self.off[i + 1]]

    @staticmethod
    def from_dict(root: dict) -> "BlobStore":
        store = BlobStore()
        store._load(NONE, "", root)
        store.finish()
        return store

    @staticmethod
    def from_json(text: str) -> "BlobStore":
        return BlobStore.from_dict(json.loads(text))

    def _load(self, parent, key, node):
        meta = {k: v for k, v in node.items() if k not in SPZ_KEYS and not _is_node(v)}
        me = self.add(parent, key, node["__spz_name__"], node["__spz_type__"], node["__spz_provider__"],
                      node["__spz_uid__"], meta)
        for k, v in node.items():
            if _is_node(v):
                self._load(me, k, v)

    def add(self, parent, key, name, kind, provider, uid, meta=None) -> int:
        me = len(self.parent)
        for column, value in zip(self.cols, (key, name, kind, provider, uid)):
            column.append(self._id(value))
        self.parent.append(NONE if parent is None else parent)
        self.meta_start.append(len(self.meta_key))
        for k, v in (meta or {}).items():
            self.meta_key.append(self._id(k))
            if type(v) is str:
                self.meta_val.append(self._id(v))
            else:
                self.meta_val.append(OTHER | len(self.others))
                self.others.append(v)
        return me

    def finish(self):
        self.blob, self._parts, self._ids = "".join(self._parts), [], None
        n = len(self.parent)
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
        self.by_uid = None

    def __len__(self):
        return len(self.parent)

    def children(self, i):
        return self.child[self.child_start[i]:self.child_start[i + 1]].tolist()

    def _value(self, v, s):
        return self.others[v & ~OTHER] if v & OTHER else s(v)

    def meta(self, i, s=None) -> dict:
        s = s or self.s
        lo, hi = self.meta_start[i], self.meta_start[i + 1]
        return {s(self.meta_key[m]): self._value(self.meta_val[m], s) for m in range(lo, hi)}

    def get(self, i, key):
        lo, hi = self.meta_start[i], self.meta_start[i + 1]
        for m in range(lo, hi):
            if self.s(self.meta_key[m]) == key:
                return self._value(self.meta_val[m], self.s)
        return None

    def _id_of(self, column, value: str) -> int:
        return next((i for i in set(column) if self.s(i) == value), NONE)

    def find_uid(self, provider, uid):
        # built on first use, once the parse garbage is gone, so its keys pack into fresh memory
        if self.by_uid is None:
            self.by_uid = {}
            for me in range(len(self) - 1, -1, -1):
                self.by_uid.setdefault(self.s(self.cols[3][me]), {})[self.s(self.cols[4][me])] = me
        return self.by_uid.get(provider, {}).get(uid)

    def find_path(self, path):
        node, key_col = 0, self.cols[0]
        for key in path.split("/"):
            node = next((c for c in self.children(node) if self.s(key_col[c]) == key), None)
            if node is None:
                return None
        return node

    def filter(self, provider=None, kind=None, contains=None):
        p = None if provider is None else self._id_of(self.cols[3], provider)
        k = None if kind is None else self._id_of(self.cols[2], kind)

        def keep(i):
            if provider is not None and self.cols[3][i] != p:
                return False
            if kind is not None and self.cols[2][i] != k:
                return False
            if contains is not None:
                value = self.get(i, contains[0])
                return type(value) is str and contains[1] in value
            return True
        return [i for i in range(len(self)) if keep(i)]

    def _cached(self):
        cache = [None] * (len(self.off) - 1)

        def s(i):
            v = cache[i]
            if v is None:
                v = cache[i] = self.s(i)
            return v
        return s

    def _to_dict(self, i, s):
        d = self.meta(i, s)
        d.update((s(self.cols[0][c]), self._to_dict(c, s)) for c in self.children(i))
        d.update(zip(SPZ_KEYS, (s(self.cols[c][i]) for c in (2, 1, 3, 4))))
        return d

    def to_json(self) -> str:
        return json.dumps(self._to_dict(0, self._cached()))

    def to_legacy(self, types: dict, default):
        s = self._cached()

        def build(i):
            cls = types.get(s(self.cols[2][i]), default)
            obj = cls.__new__(cls)
            d = obj.__dict__
            d.update(self.meta(i, s))
            for c in self.children(i):
                d[s(self.cols[0][c])] = build(c)
            d.update(zip(SPZ_KEYS, (s(self.cols[c][i]) for c in (2, 1, 3, 4))))
            return obj
        return build(0)
