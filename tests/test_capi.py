"""The C interface as a C consumer sees it (include/inventory_store.h, mirrored with ctypes): layout, snapshots, typed
values, search, the change log a Qt model replays, notifications, and lifetimes."""
import ctypes as C
import gc
import json
import os

import pytest

from inventory_store import Inventory, NativeStore
from test_inventory import loaded_dataset, loaded_server, server_segment

DATA = os.path.join(os.path.dirname(__file__), "..", "data", "inventory_v2.json")
NO_NODE, NONE, ROOT = 2 ** 64 - 1, 0xFFFFFFFF, 0
INSERTED, REMOVED, DATA_CHANGED = 1, 2, 3


class InvStr(C.Structure):
    _fields_ = [("ptr", C.c_void_p), ("len", C.c_size_t)]

    def text(self):
        return C.string_at(self.ptr, self.len).decode() if self.ptr else ""


def inv_str(s: str):
    b = s.encode()
    keep.append(b)
    return InvStr(C.cast(C.c_char_p(b), C.c_void_p), len(b))


keep = []


class InvValue(C.Structure):
    _fields_ = [("kind", C.c_uint32), ("int_value", C.c_int64), ("float_value", C.c_double), ("text", InvStr)]


class InvChange(C.Structure):
    _fields_ = [("seq", C.c_uint64), ("kind", C.c_uint32), ("position", C.c_uint32), ("count", C.c_uint32),
                ("host", C.c_uint64)]


H = S = C.c_void_p
N = C.c_uint64
NOTIFY = C.CFUNCTYPE(None, C.c_void_p)
TEXT_SINK = C.CFUNCTYPE(None, C.c_void_p, InvStr)
HIT_SINK = C.CFUNCTYPE(None, C.c_void_p, N, C.c_double)
CHANGE_SINK = C.CFUNCTYPE(None, C.c_void_p, C.POINTER(InvChange), S)


class Api(C.Structure):
    _fields_ = [("abi_version", C.c_uint32), ("struct_size", C.c_uint32), ("handle", H),
                ("retain", C.CFUNCTYPE(None, H)), ("release", C.CFUNCTYPE(None, H)),
                ("snapshot", C.CFUNCTYPE(S, H)), ("snapshot_retain", C.CFUNCTYPE(None, S)),
                ("snapshot_release", C.CFUNCTYPE(None, S)), ("snapshot_seq", C.CFUNCTYPE(C.c_uint64, S)),
                ("subscribe", C.CFUNCTYPE(C.c_uint64, H, NOTIFY, C.c_void_p)),
                ("unsubscribe", C.CFUNCTYPE(None, H, C.c_uint64)),
                ("changes_since", C.CFUNCTYPE(C.c_uint32, H, C.c_uint64, CHANGE_SINK, C.c_void_p)),
                ("child_count", C.CFUNCTYPE(C.c_uint32, S, N)), ("child", C.CFUNCTYPE(N, S, N, C.c_uint32)),
                ("parent", C.CFUNCTYPE(N, S, N)), ("row_in_parent", C.CFUNCTYPE(C.c_uint32, S, N)),
                ("find_child", C.CFUNCTYPE(N, S, N, InvStr)), ("field", C.CFUNCTYPE(InvStr, S, N, C.c_uint32)),
                ("meta_count", C.CFUNCTYPE(C.c_uint32, S, N)), ("meta_key", C.CFUNCTYPE(InvStr, S, N, C.c_uint32)),
                ("meta_value", C.CFUNCTYPE(None, S, N, C.c_uint32, C.POINTER(InvValue))),
                ("meta_find", C.CFUNCTYPE(C.c_uint32, S, N, InvStr)),
                ("meta_text", C.CFUNCTYPE(None, S, N, C.c_uint32, TEXT_SINK, C.c_void_p)),
                ("search", C.CFUNCTYPE(C.c_uint32, S, InvStr, HIT_SINK, C.c_void_p)),
                ("bm25", C.CFUNCTYPE(C.c_uint32, S, InvStr, HIT_SINK, C.c_void_p))]


_get_pointer = C.pythonapi.PyCapsule_GetPointer
_get_pointer.restype, _get_pointer.argtypes = C.c_void_p, [C.py_object, C.c_char_p]


def api_of(inv) -> Api:
    """A copy of the table, retained: what a C++ consumer keeps."""
    capsule = inv.__inventory_store_c_api__()
    mine = Api.from_buffer_copy(Api.from_address(_get_pointer(capsule, b"speasy.inventory_store.v2")))
    mine.retain(mine.handle)
    return mine


def changes(api, after):
    out = []
    api.changes_since(api.handle, after, CHANGE_SINK(lambda ctx, c, s: out.append(
        (c.contents.kind, c.contents.host, c.contents.position, c.contents.count, api.snapshot_seq(s)))), None)
    return out


@pytest.fixture(scope="module")
def big():
    inv = Inventory()
    with open(DATA) as f:
        for key, tree in json.load(f).items():
            if type(tree) is dict and "__spz_type__" in tree:
                inv.attach(key, NativeStore.from_dict(tree))
    api = api_of(inv)
    yield inv, api
    api.release(api.handle)


def test_layout_and_version(big):
    _, api = big
    assert api.abi_version == 2 and api.struct_size == C.sizeof(Api)


def test_navigation_matches_python(big):
    inv, api = big
    s = api.snapshot(api.handle)
    cda = api.find_child(s, ROOT, inv_str("cda"))
    ace = api.find_child(s, cda, inv_str("ACE"))
    assert cda == inv.find_child(ROOT, "cda") and api.field(s, cda, 0).text() == "cda"
    assert api.parent(s, ace) == cda and api.child(s, cda, api.row_in_parent(s, ace)) == ace
    assert [api.child(s, ROOT, i) for i in range(api.child_count(s, ROOT))] == inv.children(ROOT)
    assert api.parent(s, ROOT) == NO_NODE and api.child(s, ROOT, 10 ** 6) == NO_NODE
    api.snapshot_release(s)


def test_values_are_typed(big):
    inv, api = big
    s = api.snapshot(api.handle)
    p = api.find_child(s, api.find_child(s, api.find_child(s, api.find_child(s, inv.find_child(ROOT, "cda"),
                       inv_str("ACE")), inv_str("MAG")), inv_str("AC_H0_MFI")), inv_str("BGSEc"))
    v = InvValue()
    api.meta_value(s, p, api.meta_find(s, p, inv_str("UNITS")), C.byref(v))
    assert v.kind == 1 and v.text.text() == "nT"
    api.snapshot_release(s)


def test_search_through_a_callback(big):
    inv, api = big
    s, hits = api.snapshot(api.handle), []
    n = api.search(s, inv_str("provider:cda b_gse"), HIT_SINK(lambda ctx, node, score: hits.append((node, int(score)))),
                   None)
    assert n == len(hits) and sorted(hits) == sorted(inv.search("provider:cda b_gse"))
    api.snapshot_release(s)


def test_the_change_log_replays_lazy_loading_step_by_step():
    inv = Inventory()
    api = api_of(inv)
    start = inv.seq
    inv.attach("a", server_segment())
    inv.attach("hapi", server_segment())
    hapi = inv.find_child(ROOT, "hapi")
    srv = inv.find_child(hapi, "srv")
    inv.fill(srv, loaded_server()[0])
    inv.attach("a", server_segment())
    log = changes(api, start)
    assert [c[:4] for c in log] == [(INSERTED, ROOT, 0, 1), (INSERTED, ROOT, 1, 1), (INSERTED, srv, 0, 2),
                                    (DATA_CHANGED, srv, 0, 0), (REMOVED, ROOT, 0, 1), (INSERTED, ROOT, 0, 1)]
    assert [c[4] for c in log] == list(range(start + 1, start + 7))  # each step has its own snapshot
    assert changes(api, log[3][4]) == log[4:]
    api.release(api.handle)


def test_a_snapshot_never_changes():
    inv = Inventory()
    api = api_of(inv)
    inv.attach("hapi", server_segment())
    srv = inv.find_child(inv.find_child(ROOT, "hapi"), "srv")
    before = api.snapshot(api.handle)
    inv.fill(srv, loaded_server()[0])
    after = api.snapshot(api.handle)
    assert api.child_count(before, srv) == 0 and api.child_count(after, srv) == 2
    for s in (before, after):
        api.snapshot_release(s)
    api.release(api.handle)


def test_subscribers_are_notified_once_per_change():
    inv = Inventory()
    api = api_of(inv)
    calls = []
    notify = NOTIFY(lambda ctx: calls.append(1))
    token = api.subscribe(api.handle, notify, None)
    inv.attach("hapi", server_segment())
    srv = inv.find_child(inv.find_child(ROOT, "hapi"), "srv")
    inv.fill(srv, loaded_server()[0])
    api.unsubscribe(api.handle, token)
    inv.attach("x", server_segment())
    assert len(calls) == 2
    api.release(api.handle)


def test_retained_handles_outlive_python():
    inv = Inventory()
    inv.attach("hapi", server_segment())
    api = api_of(inv)
    s = api.snapshot(api.handle)
    del inv
    gc.collect()
    hapi = api.find_child(s, ROOT, inv_str("hapi"))
    assert api.child_count(s, hapi) == 2 and api.field(s, api.child(s, hapi, 0), 0).text() == "srv"
    api.snapshot_release(s)
    api.release(api.handle)
