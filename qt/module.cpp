// A Python extension holding the Qt side, loaded in the same interpreter as speasy, like SciQLopPlots in SciQLop.
// It keeps one StoreModel watched by QAbstractItemModelTester (which checks every insert/remove signal against the
// model's state), while Python makes the inventory grow: providers starting, lazy nodes filling in, refreshes.
#define PY_SSIZE_T_CLEAN
#include <Python.h>

#include "StoreModel.hpp"

#include <QAbstractItemModelTester>
#include <QCoreApplication>
#include <QElapsedTimer>
#include <QPersistentModelIndex>
#include <QStringListModel>
#include <memory>
#include <vector>

namespace
{
int g_warnings = 0;

void count_warnings(QtMsgType type, const QMessageLogContext&, const QString& message)
{
    if (type != QtDebugMsg && type != QtInfoMsg && g_warnings++ < 5)
        fprintf(stderr, "qt: %s\n", qPrintable(message));
}

struct Counters
{
    long inserted = 0, removed = 0, resets = 0, data_changed = 0;
};

struct State
{
    std::unique_ptr<StoreModel> model;
    std::unique_ptr<QAbstractItemModelTester> tester;
    std::vector<QPersistentModelIndex> pins;
    Counters counters;
};

State& state()
{
    static State s;
    return s;
}

void ensure_app()
{
    static int argc = 0;
    if (!QCoreApplication::instance())
        new QCoreApplication(argc, nullptr);
    qInstallMessageHandler(count_warnings);
}

bool take_api(PyObject* inventory, InventoryApi& out)
{
    PyObject* capsule = PyObject_CallMethod(inventory, "__inventory_store_c_api__", nullptr);
    if (!capsule)
        return false;
    auto* api = static_cast<const InventoryApi*>(PyCapsule_GetPointer(capsule, INVENTORY_STORE_CAPSULE_NAME));
    const bool ok = api && api->abi_version == INVENTORY_STORE_ABI_VERSION && api->struct_size >= sizeof(InventoryApi);
    if (ok)
        out = *api; // StoreModel retains it
    else if (!PyErr_Occurred())
        PyErr_SetString(PyExc_TypeError, "incompatible inventory C API");
    Py_DECREF(capsule);
    return ok;
}

PyObject* attach(PyObject*, PyObject* args)
{
    PyObject* inventory;
    if (!PyArg_ParseTuple(args, "O", &inventory))
        return nullptr;
    InventoryApi api;
    if (!take_api(inventory, api))
        return nullptr;
    ensure_app();
    auto& s = state();
    s.tester.reset();
    s.pins.clear();
    s.model = std::make_unique<StoreModel>(api);
    s.counters = {};
    auto* m = s.model.get();
    QObject::connect(m, &QAbstractItemModel::rowsInserted, [&s] { ++s.counters.inserted; });
    QObject::connect(m, &QAbstractItemModel::rowsRemoved, [&s] { ++s.counters.removed; });
    QObject::connect(m, &QAbstractItemModel::modelReset, [&s] { ++s.counters.resets; });
    QObject::connect(m, &QAbstractItemModel::dataChanged, [&s] { ++s.counters.data_changed; });
    g_warnings = 0;
    s.tester = std::make_unique<QAbstractItemModelTester>(m, QAbstractItemModelTester::FailureReportingMode::Warning);
    Py_RETURN_NONE;
}

PyObject* process_events(PyObject*, PyObject*)
{
    Py_BEGIN_ALLOW_THREADS QCoreApplication::processEvents();
    Py_END_ALLOW_THREADS auto& s = state();
    return Py_BuildValue("{s:l,s:l,s:l,s:l,s:i,s:i}", "inserted", s.counters.inserted, "removed", s.counters.removed,
                         "resets", s.counters.resets, "data_changed", s.counters.data_changed, "warnings",
                         g_warnings, "top_rows", s.model ? s.model->rowCount() : -1);
}

struct Walk
{
    long nodes = 0, bad_parents = 0;
};

void walk(const StoreModel& model, const QModelIndex& parent, Walk& w)
{
    const int n = model.rowCount(parent);
    for (int r = 0; r < n; ++r)
    {
        const QModelIndex i = model.index(r, 0, parent);
        (void)model.data(i, Qt::DisplayRole);
        w.bad_parents += model.parent(i) != parent;
        ++w.nodes;
        walk(model, i, w);
    }
}

PyObject* walk_model(PyObject*, PyObject*)
{
    auto& s = state();
    Walk w;
    double ms;
    Py_BEGIN_ALLOW_THREADS QElapsedTimer t;
    t.start();
    walk(*s.model, {}, w);
    ms = t.nsecsElapsed() / 1e6;
    Py_END_ALLOW_THREADS return Py_BuildValue("{s:l,s:l,s:d}", "nodes", w.nodes, "bad_parents", w.bad_parents, "ms",
                                              ms);
}

PyObject* search(PyObject*, PyObject* args)
{
    const char* query;
    if (!PyArg_ParseTuple(args, "s", &query))
        return nullptr;
    auto& s = state();
    long hits, ok;
    double ms, tooltips_ms;
    Py_BEGIN_ALLOW_THREADS QElapsedTimer t;
    t.start();
    const auto found = s.model->search(QString::fromUtf8(query));
    ms = t.nsecsElapsed() / 1e6;
    hits = found.size();
    ok = found.isEmpty() || s.model->index_of(found.first().first).internalId() == found.first().first;
    t.start();
    for (qsizetype i = 0; i < std::min<qsizetype>(found.size(), 1000); ++i)
        (void)s.model->data(s.model->index_of(found[i].first), Qt::ToolTipRole);
    tooltips_ms = t.nsecsElapsed() / 1e6;
    Py_END_ALLOW_THREADS return Py_BuildValue("{s:l,s:d,s:l,s:d}", "hits", hits, "ms", ms, "hit_index_ok", ok,
                                              "tooltips_1000_ms", tooltips_ms);
}

//! Pins the index at a key path, like a selection or an expanded node in a view. Returns the pin number, or -1.
PyObject* pin(PyObject*, PyObject* args)
{
    PyObject* keys;
    if (!PyArg_ParseTuple(args, "O", &keys))
        return nullptr;
    auto& s = state();
    QModelIndex at;
    const Py_ssize_t n = PySequence_Length(keys);
    for (Py_ssize_t k = 0; k < n; ++k)
    {
        PyObject* item = PySequence_GetItem(keys, k);
        const QString key = QString::fromUtf8(PyUnicode_AsUTF8(item));
        Py_DECREF(item);
        QModelIndex next;
        for (int r = 0; r < s.model->rowCount(at) && !next.isValid(); ++r)
            if (s.model->index(r, 0, at).data(StoreModel::KeyRole).toString() == key)
                next = s.model->index(r, 0, at);
        if (!next.isValid())
            return PyLong_FromLong(-1);
        at = next;
    }
    s.pins.emplace_back(at);
    return PyLong_FromSsize_t(Py_ssize_t(s.pins.size()) - 1);
}

//! (valid, display name, row count) of a pinned index.
PyObject* pinned(PyObject*, PyObject* args)
{
    int i;
    if (!PyArg_ParseTuple(args, "i", &i))
        return nullptr;
    const QPersistentModelIndex& p = state().pins.at(i);
    return Py_BuildValue("(Nsi)", PyBool_FromLong(p.isValid()), qPrintable(p.data().toString()),
                         p.isValid() ? state().model->rowCount(p) : -1);
}

PyObject* control_warnings(PyObject*, PyObject*)
{
    ensure_app();
    QStringListModel model({ "a", "b" });
    g_warnings = 0;
    QAbstractItemModelTester tester(&model, QAbstractItemModelTester::FailureReportingMode::Warning);
    return PyLong_FromLong(g_warnings);
}

PyObject* detach(PyObject*, PyObject*)
{
    auto& s = state();
    s.pins.clear();
    s.tester.reset();
    s.model.reset();
    Py_RETURN_NONE;
}

PyMethodDef methods[] = {
    { "attach", attach, METH_VARARGS, "attach(inventory): a StoreModel over it, watched by QAbstractItemModelTester" },
    { "process_events", process_events, METH_NOARGS, "runs queued events (model syncs); returns signal counters" },
    { "walk", walk_model, METH_NOARGS, "walks every index" },
    { "search", search, METH_VARARGS, "search(query) through the model" },
    { "pin", pin, METH_VARARGS, "pin(keys): a persistent index at a key path" },
    { "pinned", pinned, METH_VARARGS, "pinned(i) -> (valid, display, rows)" },
    { "control_warnings", control_warnings, METH_NOARGS, "tester warnings on a stock QStringListModel" },
    { "detach", detach, METH_NOARGS, "drops the model" },
    { nullptr, nullptr, 0, nullptr },
};

PyModuleDef module = { PyModuleDef_HEAD_INIT, "store_qt", nullptr, -1, methods };
} // namespace

PyMODINIT_FUNC PyInit_store_qt()
{
    return PyModule_Create(&module);
}
