"""The database-like C interface as a C consumer sees it (inventory_db.h, mirrored with ctypes). Besides normal
use, hostile input at every entry point: wrong connections, unpinned versions, random node ids, invalid UTF-8, null
out-pointers and callbacks, small buffers. Every call must return a status; none may crash the process."""
import ctypes as C
import os
import random

import pytest

import fixtures
import speasy_inventory_store
from speasy_inventory_store import Inventory, NativeStore
from test_inventory import loaded_server, server_segment

OK, NOT_FOUND, BAD_CONNECTION, BAD_VERSION, BAD_ARGUMENT, BUFFER_TOO_SMALL, RESET_NEEDED, INTERNAL = range(8)
ROOT, NO_NODE, CURRENT = 0, 2 ** 64 - 1, 2 ** 64 - 1
DISPLAY, TOOLTIP = 5, 6


class InvDbStr(C.Structure):
    _fields_ = [("ptr", C.c_void_p), ("len", C.c_size_t)]


class InvDbValue(C.Structure):
    _fields_ = [("kind", C.c_uint32), ("int_value", C.c_int64), ("float_value", C.c_double), ("text", InvDbStr)]


class NodeInfo(C.Structure):
    _fields_ = [("parent", C.c_uint64), ("row", C.c_uint32), ("child_count", C.c_uint32)]


class Change(C.Structure):
    _fields_ = [("version", C.c_uint64), ("kind", C.c_uint32), ("position", C.c_uint32), ("count", C.c_uint32),
                ("host", C.c_uint64)]


U64, I32, P = C.c_uint64, C.c_int32, C.c_void_p
NOTIFY = C.CFUNCTYPE(None, P)
TEXT = C.CFUNCTYPE(None, P, C.c_void_p, C.c_size_t)
META = C.CFUNCTYPE(None, P, C.c_void_p, C.c_size_t, C.POINTER(InvDbValue))
HITS = C.CFUNCTYPE(None, P, C.POINTER(U64), C.POINTER(C.c_double), C.c_uint32)
CHANGE = C.CFUNCTYPE(None, P, C.POINTER(Change))


class Api(C.Structure):
    pass


Api._fields_ = [("abi_version", C.c_uint32), ("struct_size", C.c_uint32),
                ("connect", C.CFUNCTYPE(I32, C.POINTER(Api), C.POINTER(U64))), ("disconnect", C.CFUNCTYPE(I32, U64)),
                ("current_version", C.CFUNCTYPE(I32, U64, C.POINTER(U64))), ("pin", C.CFUNCTYPE(I32, U64, U64)),
                ("unpin", C.CFUNCTYPE(I32, U64, U64)),
                ("subscribe", C.CFUNCTYPE(I32, U64, NOTIFY, P, C.POINTER(U64))),
                ("unsubscribe", C.CFUNCTYPE(I32, U64, U64)), ("changes", C.CFUNCTYPE(I32, U64, U64, CHANGE, P)),
                ("node", C.CFUNCTYPE(I32, U64, U64, U64, C.POINTER(NodeInfo))),
                ("child", C.CFUNCTYPE(I32, U64, U64, U64, C.c_uint32, C.POINTER(U64))),
                ("children", C.CFUNCTYPE(I32, U64, U64, U64, C.POINTER(U64), C.c_uint32, C.POINTER(C.c_uint32))),
                ("find_child", C.CFUNCTYPE(I32, U64, U64, U64, C.c_char_p, C.c_size_t, C.POINTER(U64))),
                ("text", C.CFUNCTYPE(I32, U64, U64, U64, C.c_uint32, TEXT, P)),
                ("meta", C.CFUNCTYPE(I32, U64, U64, U64, C.c_char_p, C.c_size_t, META, P)),
                ("meta_all", C.CFUNCTYPE(I32, U64, U64, U64, META, P)),
                ("search", C.CFUNCTYPE(I32, U64, U64, C.c_char_p, C.c_size_t, HITS, P)),
                ("bm25", C.CFUNCTYPE(I32, U64, U64, C.c_char_p, C.c_size_t, HITS, P)), ("reserved", P)]

_get_pointer = C.pythonapi.PyCapsule_GetPointer
_get_pointer.restype, _get_pointer.argtypes = C.c_void_p, [C.py_object, C.c_char_p]
NULL_TEXT, NULL_META, NULL_HITS, NULL_CHANGE, NULL_NOTIFY = TEXT(), META(), HITS(), CHANGE(), NOTIFY()


def connect(inv):
    capsule = inv.__inventory_db_c_api__()
    api = Api.from_address(_get_pointer(capsule, b"speasy.inventory_db.v1"))
    conn = U64()
    assert api.connect(C.byref(api), C.byref(conn)) == OK
    return Api.from_buffer_copy(api), conn.value  # the capsule may go now: only integers are kept


def text(api, conn, version, node, which):
    out = []
    status = api.text(conn, version, node, which, TEXT(lambda ctx, p, n: out.append(C.string_at(p, n).decode())), None)
    return status, (out[0] if out else None)


def b(s):
    return s.encode()


@pytest.fixture(scope="module")
def big():
    inv = Inventory()
    for key, tree in fixtures.inventory().items():
        if type(tree) is dict and "__spz_type__" in tree:
            inv.attach(key, NativeStore.from_dict(tree))
    api, conn = connect(inv)
    v = U64()
    api.current_version(conn, C.byref(v))
    api.pin(conn, v.value)
    yield inv, api, conn, v.value
    api.disconnect(conn)


def test_normal_use(big):
    inv, api, conn, v = big
    cda = U64()
    assert api.find_child(conn, v, ROOT, b"cda", 3, C.byref(cda)) == OK and cda.value == inv.find_child(ROOT, "cda")
    n = C.c_uint32()
    assert api.children(conn, v, ROOT, None, 0, C.byref(n)) == BUFFER_TOO_SMALL  # asks the size first
    buffer = (U64 * n.value)()
    assert api.children(conn, v, ROOT, buffer, n.value, C.byref(n)) == OK and list(buffer) == inv.children(ROOT)
    info = NodeInfo()
    assert api.node(conn, v, cda.value, C.byref(info)) == OK and info.parent == ROOT
    assert text(api, conn, v, cda.value, DISPLAY) == (OK, "cda")
    status, tip = text(api, conn, v, inv.find_child(inv.find_child(cda.value, "ACE"), "MAG"), TOOLTIP)
    assert status == OK and tip.startswith("<h3>")
    hits = []
    assert api.search(conn, v, b"provider:cda b_gse", 18, HITS(lambda ctx, nodes, scores, k: hits.extend(
        (nodes[i], int(scores[i])) for i in range(k))), None) == OK
    assert sorted(hits) == sorted(inv.search("provider:cda b_gse"))


def test_hostile_input_returns_a_status(big):
    inv, api, conn, v = big
    rng = random.Random(1)
    node_ids = [rng.getrandbits(64) for _ in range(200)] + [NO_NODE, 1 << 32, (1 << 32) | 10 ** 9]
    info, out, n = NodeInfo(), U64(), C.c_uint32()
    for node in node_ids:
        assert api.node(conn, v, node, C.byref(info)) in (OK, NOT_FOUND)
        assert api.child(conn, v, node, rng.getrandbits(32), C.byref(out)) == NOT_FOUND
        assert text(api, conn, v, node, rng.randrange(8))[0] in (OK, NOT_FOUND, BAD_ARGUMENT)
        assert api.children(conn, v, node, None, 0, C.byref(n)) in (OK, BUFFER_TOO_SMALL, NOT_FOUND)
    assert api.node(conn, 12345, ROOT, C.byref(info)) == BAD_VERSION  # never pinned
    assert api.node(999999, v, ROOT, C.byref(info)) == BAD_CONNECTION
    assert api.node(conn, v, ROOT, None) == BAD_ARGUMENT  # null out-pointer
    assert api.children(conn, v, ROOT, None, 5, C.byref(n)) == BAD_ARGUMENT  # null buffer with a capacity
    assert api.find_child(conn, v, ROOT, b"\xff\xfe", 2, C.byref(out)) == BAD_ARGUMENT  # invalid UTF-8
    assert api.find_child(conn, v, ROOT, None, 5, C.byref(out)) == BAD_ARGUMENT  # null text with a length
    assert api.text(conn, v, ROOT, DISPLAY, NULL_TEXT, None) == BAD_ARGUMENT  # null callbacks
    assert api.search(conn, v, b"x", 1, NULL_HITS, None) == BAD_ARGUMENT
    assert api.meta_all(conn, v, ROOT, NULL_META, None) == BAD_ARGUMENT
    assert api.changes(conn, 0, NULL_CHANGE, None) == BAD_ARGUMENT
    assert api.subscribe(conn, NULL_NOTIFY, None, C.byref(out)) == BAD_ARGUMENT
    assert api.unpin(conn, 12345) == BAD_VERSION
    assert api.pin(conn, 10 ** 12) == BAD_VERSION  # no such version
    assert api.connect(None, C.byref(out)) == BAD_ARGUMENT


def test_use_after_disconnect_is_an_error():
    inv = Inventory()
    inv.attach("hapi", server_segment())
    api, conn = connect(inv)
    del inv  # the connection keeps the inventory alive, not Python
    v, info = U64(), NodeInfo()
    assert api.current_version(conn, C.byref(v)) == OK and api.pin(conn, v.value) == OK
    assert api.node(conn, v.value, ROOT, C.byref(info)) == OK and info.child_count == 1
    assert api.disconnect(conn) == OK
    assert api.disconnect(conn) == BAD_CONNECTION
    assert api.node(conn, v.value, ROOT, C.byref(info)) == BAD_CONNECTION
    assert api.pin(conn, v.value) == BAD_CONNECTION


def test_changes_and_versions_follow_lazy_loading():
    inv = Inventory()
    api, conn = connect(inv)
    start = U64()
    api.current_version(conn, C.byref(start))
    api.pin(conn, start.value)
    calls = []
    notify, token = NOTIFY(lambda ctx: calls.append(1)), U64()
    assert api.subscribe(conn, notify, None, C.byref(token)) == OK
    inv.attach("hapi", server_segment())
    srv = inv.find_child(inv.find_child(ROOT, "hapi"), "srv")
    inv.fill(srv, loaded_server()[0])
    log = []
    assert api.changes(conn, start.value, CHANGE(lambda ctx, c: log.append(
        (c.contents.kind, c.contents.host, c.contents.position, c.contents.count, c.contents.version))), None) == OK
    assert [c[:4] for c in log] == [(1, ROOT, 0, 1), (1, srv, 0, 2), (3, srv, 0, 0)] and len(calls) == 2
    info = NodeInfo()
    for _, _, _, _, version in log:  # each step's version can be pinned and read
        assert api.pin(conn, version) == OK
    assert api.node(conn, log[0][4], srv, C.byref(info)) == OK and info.child_count == 0
    assert api.node(conn, log[1][4], srv, C.byref(info)) == OK and info.child_count == 2
    assert api.node(conn, start.value, srv, C.byref(info)) == NOT_FOUND  # before hapi existed
    api.disconnect(conn)


def test_installed_header_matches_the_binary():
    with open(os.path.join(speasy_inventory_store.get_include(), "inventory_db.h")) as f:
        header = f.read()
    api, conn = connect(Inventory())
    assert f"#define INVENTORY_DB_ABI_VERSION {api.abi_version}u" in header
    assert '#define INVENTORY_DB_CAPSULE_NAME "speasy.inventory_db.v1"' in header
    api.disconnect(conn)
