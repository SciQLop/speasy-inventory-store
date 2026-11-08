"""Both stores must give back exactly what speasy's tree gives, on the real inventory."""
import copy
import datetime
import json
import math
import os
import pickle

import numpy as np

import pytest
from speasy.core.inventory.indexes import SpeasyIndex, __INDEXES_TYPES__, from_dict

import fixtures
from speasy_inventory_store import NativeStore

LIVE = os.path.join(os.path.dirname(__file__), "..", "data", "live_trees.pkl")
STORES = [NativeStore]


@pytest.fixture(scope="module")
def inventory() -> dict:
    return fixtures.inventory()


@pytest.fixture(scope="module", params=STORES, ids=lambda s: s.__name__)
def store(request, inventory):
    return request.param.from_dict(copy.deepcopy(inventory))


def build_with_add(cls, root: dict):
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


def is_node(v):
    return type(v) is dict and "__spz_type__" in v


def test_json_export_gives_the_input_back(store, inventory):
    assert json.loads(store.to_json()) == inventory


@pytest.mark.parametrize("cls", STORES, ids=lambda s: s.__name__)
def test_json_import_equals_dict_import(cls, inventory):
    assert same(json.loads(cls.from_json(json.dumps(inventory)).to_json()), inventory)


@pytest.mark.parametrize("cls", STORES, ids=lambda s: s.__name__)
def test_builder_api_equals_dict_import(cls, inventory):
    assert json.loads(build_with_add(cls, inventory).to_json()) == inventory


def test_legacy_tree_equals_speasy_tree(store, inventory):
    legacy = store.to_legacy(dict(__INDEXES_TYPES__), SpeasyIndex)
    assert same(legacy, from_dict(copy.deepcopy(inventory), version=2))


def test_lookups(store):
    i = store.find_uid("cda", "AC_H0_MFI/BGSEc")
    assert i is not None
    assert store.find_path("cda/ACE/MAG/AC_H0_MFI/BGSEc") == i
    assert store.find_path("cda/nowhere") is None
    assert store.find_uid("cda", "nowhere") is None


def test_both_stores_filter_alike(inventory):
    query = dict(provider="cda", kind="ParameterIndex", contains=("UNITS", "nT"))
    hits = [cls.from_dict(copy.deepcopy(inventory)).filter(**query) for cls in STORES]
    assert hits[0] != [] and all(h == hits[0] for h in hits)


VALUES = {"str": "a", "int": 3, "big_int": 2 ** 70, "float": 2.5, "nan": float("nan"), "bool": True, "none": None,
          "list": [1, 2.0, "a"], "tuple": (3,), "choices": [("Anode 1", "0"), ("Anode 2", "1")],
          "dict": {"a": {"b": (1, None)}}, "int_keys": {1: "a"}, "set": {1}, "np_float32": np.float32(1.5),
          "np_int64": np.int64(7), "datetime": datetime.datetime(2020, 1, 1)}


def same(a, b):
    """Equal with exact types, NaN equal to NaN: speasy's == can't tell (1,) from [1] in a dict, nor match NaNs."""
    if type(a) is not type(b):
        return False
    if isinstance(a, SpeasyIndex):
        return same(a.__dict__, b.__dict__)
    if isinstance(a, float) and math.isnan(a):
        return math.isnan(b)
    if isinstance(a, (list, tuple)):
        return len(a) == len(b) and all(same(x, y) for x, y in zip(a, b))
    if isinstance(a, dict):
        return a.keys() == b.keys() and all(same(a[k], b[k]) for k in a)
    return a == b


@pytest.mark.parametrize("cls", STORES, ids=lambda s: s.__name__)
def test_values_come_back_with_their_type(cls):
    store = cls()
    store.add(None, "", "root", "SpeasyIndex", "p", "", VALUES)
    store.finish()
    meta = store.meta(0)
    legacy = store.to_legacy(dict(__INDEXES_TYPES__), SpeasyIndex).__dict__
    for key, value in VALUES.items():
        assert same(meta[key], value), key
        assert same(store.get(0, key), value), key
        assert same(legacy[key], value), key


def from_tree(cls, root: SpeasyIndex):
    store = cls()

    def walk(parent, key, node):
        d = node.__dict__
        meta = {k: v for k, v in d.items() if not k.startswith("__spz_") and not isinstance(v, SpeasyIndex)}
        me = store.add(parent, key, node.spz_name(), node.spz_type(), node.spz_provider(), node.spz_uid(), meta)
        for k, v in d.items():
            if isinstance(v, SpeasyIndex):
                walk(me, k, v)
    walk(None, "", root)
    store.finish()
    return store


@pytest.fixture(scope="module")
def live_trees() -> dict:
    """Each provider's tree as speasy builds it locally (no proxy), saved by the type survey."""
    if not os.path.exists(LIVE):
        pytest.skip("no data/live_trees.pkl")
    with open(LIVE, "rb") as f:
        return pickle.load(f)


@pytest.mark.parametrize("cls", STORES, ids=lambda s: s.__name__)
def test_trees_built_by_speasy_come_back_equal(cls, live_trees):
    for provider, tree in live_trees.items():
        legacy = from_tree(cls, tree).to_legacy(dict(__INDEXES_TYPES__), SpeasyIndex)
        assert same(legacy, tree), provider
