"""An inventory made of segments must look and search exactly like one store holding everything, while providers
attach one by one, lazy nodes fill in, and providers get replaced."""
import math

import pytest

import fixtures
from speasy_inventory_store import Inventory, NativeStore

ROOT = 0


@pytest.fixture(scope="module")
def whole():
    return fixtures.inventory()


def is_node(v):
    return type(v) is dict and "__spz_type__" in v


def providers(whole):
    return [(k, v) for k, v in whole.items() if is_node(v)]


@pytest.fixture(scope="module")
def single(whole):
    s = NativeStore.from_dict(whole)
    s.build_search()
    return s


@pytest.fixture(scope="module")
def inventory(whole):
    inv = Inventory()
    for key, tree in providers(whole):
        inv.attach(key, NativeStore.from_dict(tree))
    return inv


def walk_inventory(inv, node=ROOT, path=()):
    for child in inv.children(node):
        p = path + (inv.key(child),)
        yield p, child
        yield from walk_inventory(inv, child, p)


def walk_store(store, row=0, path=()):
    for child in store.children(row):
        p = path + (store.key(child),)
        yield p, child
        yield from walk_store(store, child, p)


def test_same_tree_as_one_store(single, segmented):
    inv, segments = segmented
    expected = [(p, single.kind(r), single.uid(r)) for p, r in walk_store(single)]
    got = []
    for p, n in walk_inventory(inv):
        seg, row = inv.resolve(n)
        got.append((p, segments[seg].kind(row), segments[seg].uid(row)))
    assert got == expected


def test_parents_and_rows_are_consistent(inventory):
    for _, n in list(walk_inventory(inventory))[::97]:
        parent = inventory.parent(n)
        assert inventory.children(parent)[inventory.row_in_parent(n)] == n


def by_uid_single(single, hits):
    return {(single.provider(r), single.uid(r)): s for r, s in hits}


def by_uid_inventory(inv, segments, hits):
    out = {}
    for n, s in hits:
        seg, row = inv.resolve(n)
        st = segments[seg]
        out[(st.provider(row), st.uid(row))] = s
    return out


@pytest.fixture(scope="module")
def segmented(whole):
    """An inventory plus its NativeStores by segment, to read hits back."""
    inv, segments = Inventory(), {}
    for key, tree in providers(whole):
        st = NativeStore.from_dict(tree)
        segments[inv.attach(key, st)] = st
    return inv, segments


@pytest.mark.parametrize("query", ["mms1 fgm bgse", "provider:cda after:2020 b_gse", "magnetic field", "§§§"])
def test_fuzzy_search_equals_one_store(single, segmented, query):
    inv, segments = segmented
    assert by_uid_inventory(inv, segments, inv.search(query)) == by_uid_single(single, single.search(query))


@pytest.mark.parametrize("query", ["mms fgm b gse", "solar wind proton density", "electron pitch angle"])
def test_bm25_equals_one_store_with_corpus_wide_statistics(single, segmented, query):
    inv, segments = segmented
    got, expected = by_uid_inventory(inv, segments, inv.bm25(query)), by_uid_single(single, single.bm25(query))
    assert got.keys() == expected.keys()
    assert all(math.isclose(got[k], v, rel_tol=1e-9) for k, v in expected.items())


def server_segment():
    """A provider like speasy's HAPI one: servers are childless until loaded."""
    s = NativeStore()
    s.add(None, "", "hapi", "SpeasyIndex", "hapi", "hapi")
    s.add(0, "srv", "srv", "HapiServerIndex", "hapi", "srv", {"hapi_loaded": False, "x_url": "https://x"})
    s.add(0, "other", "other", "HapiServerIndex", "hapi", "other", {"hapi_loaded": False})
    s.finish()
    return s


def loaded_server():
    """What load_into builds: the same node, now loaded, with its datasets."""
    s = NativeStore()
    s.add(None, "", "srv", "HapiServerIndex", "hapi", "srv", {"hapi_loaded": True, "x_url": "https://x",
                                                             "description": "A server"})
    ds = s.add(0, "ds", "ds", "HapiDatasetIndex", "hapi", "srv/ds", {"hapi_loaded": False, "title": "magnetic"})
    s.add(0, "ds2", "ds2", "HapiDatasetIndex", "hapi", "srv/ds2", {"hapi_loaded": False})
    s.finish()
    return s, ds


def loaded_dataset():
    s = NativeStore()
    s.add(None, "", "ds", "HapiDatasetIndex", "hapi", "srv/ds", {"hapi_loaded": True, "start_date": "2000-01-01T00:00Z"})
    s.add(0, "Bx", "Bx", "ParameterIndex", "hapi", "srv/ds/Bx", {"units": "nT"})
    s.finish()
    return s


def test_lazy_nodes_fill_in_place():
    inv = Inventory()
    hapi = server_segment()
    inv.attach("hapi", hapi)
    top = inv.find_child(ROOT, "hapi")
    srv = inv.find_child(top, "srv")
    assert inv.children(srv) == []
    server, ds_row = loaded_server()
    inv.fill(srv, server)
    assert [inv.key(c) for c in inv.children(srv)] == ["ds", "ds2"]
    assert inv.key(srv) == "srv" and inv.find_child(top, "srv") == srv  # same node, same id
    ds = inv.find_child(srv, "ds")
    assert inv.parent(ds) == srv and inv.row_in_parent(ds) == 0
    inv.fill(ds, loaded_dataset())
    bx = inv.find_child(ds, "Bx")
    assert inv.parent(bx) == ds and inv.parent(inv.parent(bx)) == srv
    hits = inv.search("srv ds Bx")
    assert bx in dict(hits)


def test_fill_refuses_what_isnt_a_lazy_node():
    inv = Inventory()
    inv.attach("hapi", server_segment())
    top = inv.find_child(ROOT, "hapi")
    with pytest.raises(ValueError, match="has children"):
        inv.fill(top, loaded_server()[0])
    srv = inv.find_child(top, "srv")
    inv.fill(srv, loaded_server()[0])
    with pytest.raises(ValueError, match="already filled"):
        inv.fill(srv, loaded_server()[0])


def test_a_filled_node_search_text_includes_its_ancestors():
    inv = Inventory()
    inv.attach("hapi", server_segment())
    srv = inv.find_child(inv.find_child(ROOT, "hapi"), "srv")
    inv.fill(srv, loaded_server()[0])
    ds = inv.find_child(srv, "ds")
    inv.fill(ds, loaded_dataset())
    assert inv.search("hapi srv Bx") and inv.search("A server")  # the server's description is inherited


def test_replacing_a_provider_drops_its_fills_and_keeps_its_place():
    inv = Inventory()
    inv.attach("a", server_segment())
    inv.attach("hapi", server_segment())
    inv.attach("z", server_segment())
    srv = inv.find_child(inv.find_child(ROOT, "hapi"), "srv")
    inv.fill(srv, loaded_server()[0])
    inv.attach("hapi", server_segment())
    assert [k for k, _ in inv.top()] == ["a", "hapi", "z"]
    srv2 = inv.find_child(inv.find_child(ROOT, "hapi"), "srv")
    assert srv2 != srv and inv.children(srv2) == []
    assert inv.resolve(srv) is None  # the old node is gone with its segment


def test_each_change_is_one_step():
    inv = Inventory()
    seq = inv.seq
    inv.attach("a", server_segment())
    inv.attach("a", server_segment())  # replaced: removed then inserted
    assert inv.seq == seq + 3


def test_mounting_shares_the_store():
    inv, s = Inventory(), server_segment()
    inv.attach("x", s)
    with pytest.raises(ValueError, match="shared"):
        s.add(0, "y", "y", "SpeasyIndex", "p", "y")
