"""The C interface as a C consumer sees it (include/inventory_store.h, mirrored with ctypes): layout, typed values,
search through a callback, and lifetime: a retained store outlives the Python object and the capsule."""
import ctypes as C
import gc
import os

import pytest

from inventory_store import NativeStore

DATA = os.path.join(os.path.dirname(__file__), "..", "data", "inventory_v2.json")
NONE = 0xFFFFFFFF


class InvStr(C.Structure):
    _fields_ = [("ptr", C.c_void_p), ("len", C.c_size_t)]

    def text(self):
        return C.string_at(self.ptr, self.len).decode() if self.ptr else ""

    @staticmethod
    def of(s: str):
        b = s.encode()
        return InvStr(C.cast(C.c_char_p(b), C.c_void_p), len(b)), b  # keep b alive with the struct


class InvValue(C.Structure):
    _fields_ = [("kind", C.c_uint32), ("int_value", C.c_int64), ("float_value", C.c_double), ("text", InvStr)]


H = C.c_void_p
TEXT_SINK = C.CFUNCTYPE(None, C.c_void_p, InvStr)
HIT_SINK = C.CFUNCTYPE(None, C.c_void_p, C.c_uint32, C.c_double)


class Api(C.Structure):
    _fields_ = [("abi_version", C.c_uint32), ("struct_size", C.c_uint32), ("handle", H),
                ("retain", C.CFUNCTYPE(None, H)), ("release", C.CFUNCTYPE(None, H)),
                ("generation", C.CFUNCTYPE(C.c_uint64, H)), ("node_count", C.CFUNCTYPE(C.c_uint32, H)),
                ("child_count", C.CFUNCTYPE(C.c_uint32, H, C.c_uint32)),
                ("child", C.CFUNCTYPE(C.c_uint32, H, C.c_uint32, C.c_uint32)),
                ("parent", C.CFUNCTYPE(C.c_uint32, H, C.c_uint32)),
                ("row_in_parent", C.CFUNCTYPE(C.c_uint32, H, C.c_uint32)),
                ("find_child", C.CFUNCTYPE(C.c_uint32, H, C.c_uint32, InvStr)),
                ("field", C.CFUNCTYPE(InvStr, H, C.c_uint32, C.c_uint32)),
                ("meta_count", C.CFUNCTYPE(C.c_uint32, H, C.c_uint32)),
                ("meta_key", C.CFUNCTYPE(InvStr, H, C.c_uint32, C.c_uint32)),
                ("meta_value", C.CFUNCTYPE(None, H, C.c_uint32, C.c_uint32, C.POINTER(InvValue))),
                ("meta_find", C.CFUNCTYPE(C.c_uint32, H, C.c_uint32, InvStr)),
                ("meta_text", C.CFUNCTYPE(None, H, C.c_uint32, C.c_uint32, TEXT_SINK, C.c_void_p)),
                ("search", C.CFUNCTYPE(C.c_uint32, H, InvStr, HIT_SINK, C.c_void_p)),
                ("bm25", C.CFUNCTYPE(C.c_uint32, H, InvStr, HIT_SINK, C.c_void_p))]


_get_pointer = C.pythonapi.PyCapsule_GetPointer
_get_pointer.restype, _get_pointer.argtypes = C.c_void_p, [C.py_object, C.c_char_p]


def api_of(store) -> Api:
    """A copy of the table, retained: what a C++ consumer keeps."""
    capsule = store.__inventory_store_c_api__()
    api = Api.from_address(_get_pointer(capsule, b"speasy.inventory_store.v1"))
    mine = Api.from_buffer_copy(api)
    mine.retain(mine.handle)
    return mine


@pytest.fixture(scope="module")
def store():
    return NativeStore.from_json(open(DATA).read())


@pytest.fixture(scope="module")
def api(store):
    a = api_of(store)
    yield a
    a.release(a.handle)


def test_layout_and_version(api):
    assert api.abi_version == 1
    assert api.struct_size == C.sizeof(Api)


def test_navigation_matches_the_python_view(store, api):
    h = api.handle
    assert api.node_count(h) == len(store)
    cda = api.find_child(h, 0, InvStr.of("cda")[0])
    ace = api.find_child(h, cda, InvStr.of("ACE")[0])
    assert api.field(h, ace, 0).text() == "ACE" and api.parent(h, ace) == cda
    assert api.child(h, cda, api.row_in_parent(h, ace)) == ace
    assert [api.child(h, cda, i) for i in range(api.child_count(h, cda))] == store.children(cda)
    assert api.parent(h, 0) == NONE and api.child(h, 0, 10 ** 6) == NONE and api.field(h, NONE, 0).text() == ""


def test_values_are_typed(store, api):
    h = api.handle
    row = store.find_uid("cda", "AC_H0_MFI/BGSEc")
    expected = store.meta(row)
    for i in range(api.meta_count(h, row)):
        key, value = api.meta_key(h, row, i).text(), InvValue()
        api.meta_value(h, row, i, C.byref(value))
        if isinstance(expected[key], str):
            assert value.kind == 1 and value.text.text() == expected[key]
        elif isinstance(expected[key], list):
            assert value.kind == 6
            texts = []
            api.meta_text(h, row, i, TEXT_SINK(lambda ctx, s: texts.append(s.text())), None)
            assert texts and texts[0].startswith("[")
    units = api.meta_find(h, row, InvStr.of("UNITS")[0])
    v = InvValue()
    api.meta_value(h, row, units, C.byref(v))
    assert v.text.text() == "nT"


def test_search_through_a_callback(store, api):
    hits = []
    count = api.search(api.handle, InvStr.of("provider:cda b_gse")[0],
                       HIT_SINK(lambda ctx, row, score: hits.append((row, int(score)))), None)
    store.build_search()
    assert count == len(hits) and sorted(hits) == sorted(store.search("provider:cda b_gse"))


def test_a_retained_store_outlives_the_python_object():
    store = NativeStore.from_json(open(DATA).read())
    n, generation = len(store), store.generation
    mine = api_of(store)
    del store
    gc.collect()
    assert mine.node_count(mine.handle) == n and mine.generation(mine.handle) == generation
    assert mine.field(mine.handle, 0, 1).text() == "all"
    mine.release(mine.handle)


def test_a_shared_store_cant_change():
    store = NativeStore()
    store.add(None, "", "root", "SpeasyIndex", "p", "")
    mine = api_of(store)
    with pytest.raises(ValueError, match="shared through its C API"):
        store.add(0, "x", "x", "SpeasyIndex", "p", "x")
    mine.release(mine.handle)
    gc.collect()
    store.add(0, "x", "x", "SpeasyIndex", "p", "x")  # back to one owner: building may go on


def test_each_store_is_its_own_generation(store):
    assert NativeStore().generation != store.generation
