"""Both stores must give back exactly what speasy's tree gives, on the real inventory."""
import copy
import json
import os

import pytest
from speasy.core.inventory.indexes import SpeasyIndex, __INDEXES_TYPES__, from_dict

from inventory_store import BlobStore, NativeStore, PyStore

DATA = os.path.join(os.path.dirname(__file__), "..", "data", "inventory.json")
STORES = [PyStore, BlobStore, NativeStore]


@pytest.fixture(scope="module")
def inventory() -> dict:
    with open(DATA) as f:
        return json.load(f)


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
    assert json.loads(cls.from_json(json.dumps(inventory)).to_json()) == inventory


@pytest.mark.parametrize("cls", STORES, ids=lambda s: s.__name__)
def test_builder_api_equals_dict_import(cls, inventory):
    assert json.loads(build_with_add(cls, inventory).to_json()) == inventory


def test_legacy_tree_equals_speasy_tree(store, inventory):
    legacy = store.to_legacy(dict(__INDEXES_TYPES__), SpeasyIndex)
    assert legacy == from_dict(copy.deepcopy(inventory), version=2)


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
