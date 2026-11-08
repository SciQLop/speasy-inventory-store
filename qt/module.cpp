// A Python extension holding the Qt side, loaded in the same interpreter as speasy, like SciQLopPlots in SciQLop.
// It keeps one DbModel (over the database-like C interface, inventory_db.h) watched by QAbstractItemModelTester (which checks every insert/remove signal against the
// model's state), while Python makes the inventory grow: providers starting, lazy nodes filling in, refreshes.
#define PY_SSIZE_T_CLEAN
#include <Python.h>

#include "DbModel.hpp"

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

void ensure_app()
{
    static int argc = 0;
    if (!QCoreApplication::instance())
        new QCoreApplication(argc, nullptr);
    qInstallMessageHandler(count_warnings);
}
struct Counters
{
    long inserted = 0, removed = 0, resets = 0, data_changed = 0;
};

struct DbState
{
    std::unique_ptr<DbModel> model;
    std::unique_ptr<QAbstractItemModelTester> tester;
    std::vector<QPersistentModelIndex> pins;
    Counters counters;
};

DbState& db()
{
    static DbState s;
    return s;
}

struct Walk
{
    long nodes = 0, bad_parents = 0;
};

void walk(const DbModel& model, const QModelIndex& parent, Walk& w)
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


PyObject* attach(PyObject*, PyObject* args)
{
    PyObject* inventory;
    if (!PyArg_ParseTuple(args, "O", &inventory))
        return nullptr;
    PyObject* capsule = PyObject_CallMethod(inventory, "__inventory_db_c_api__", nullptr);
    if (!capsule)
        return nullptr;
    auto* api = static_cast<const InventoryDbApi*>(PyCapsule_GetPointer(capsule, INVENTORY_DB_CAPSULE_NAME));
    if (!api || api->abi_version != INVENTORY_DB_ABI_VERSION || api->struct_size < sizeof(InventoryDbApi))
    {
        Py_DECREF(capsule);
        if (!PyErr_Occurred())
            PyErr_SetString(PyExc_TypeError, "incompatible inventory db C API");
        return nullptr;
    }
    ensure_app();
    auto& s = db();
    s.tester.reset();
    s.pins.clear();
    s.model = std::make_unique<DbModel>(api); // connects while the capsule is alive
    Py_DECREF(capsule);
    s.counters = {};
    auto* m = s.model.get();
    QObject::connect(m, &QAbstractItemModel::rowsInserted, [&s] { ++s.counters.inserted; });
    QObject::connect(m, &QAbstractItemModel::rowsRemoved, [&s] { ++s.counters.removed; });
    QObject::connect(m, &QAbstractItemModel::modelReset, [&s] { ++s.counters.resets; });
    QObject::connect(m, &QAbstractItemModel::dataChanged, [&s] { ++s.counters.data_changed; });
    g_warnings = 0;
    s.tester = std::make_unique<QAbstractItemModelTester>(m, QAbstractItemModelTester::FailureReportingMode::Warning);
    return PyBool_FromLong(m->connected());
}

PyObject* process_events(PyObject*, PyObject*)
{
    Py_BEGIN_ALLOW_THREADS QCoreApplication::processEvents();
    Py_END_ALLOW_THREADS auto& s = db();
    return Py_BuildValue("{s:l,s:l,s:l,s:l,s:i,s:i,s:l}", "inserted", s.counters.inserted, "removed",
                         s.counters.removed, "resets", s.counters.resets, "data_changed", s.counters.data_changed,
                         "warnings", g_warnings, "top_rows", s.model ? s.model->rowCount() : -1, "errors",
                         s.model ? s.model->errors() : -1);
}

PyObject* walk(PyObject*, PyObject*)
{
    auto& s = db();
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
    auto& s = db();
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

PyObject* pin(PyObject*, PyObject* args)
{
    PyObject* keys;
    if (!PyArg_ParseTuple(args, "O", &keys))
        return nullptr;
    auto& s = db();
    QModelIndex at;
    const Py_ssize_t n = PySequence_Length(keys);
    for (Py_ssize_t k = 0; k < n; ++k)
    {
        PyObject* item = PySequence_GetItem(keys, k);
        const QString key = QString::fromUtf8(PyUnicode_AsUTF8(item));
        Py_DECREF(item);
        QModelIndex next;
        for (int r = 0; r < s.model->rowCount(at) && !next.isValid(); ++r)
            if (s.model->index(r, 0, at).data(DbModel::KeyRole).toString() == key)
                next = s.model->index(r, 0, at);
        if (!next.isValid())
            return PyLong_FromLong(-1);
        at = next;
    }
    s.pins.emplace_back(at);
    return PyLong_FromSsize_t(Py_ssize_t(s.pins.size()) - 1);
}

PyObject* pinned(PyObject*, PyObject* args)
{
    int i;
    if (!PyArg_ParseTuple(args, "i", &i))
        return nullptr;
    const QPersistentModelIndex& p = db().pins.at(i);
    return Py_BuildValue("(Nsi)", PyBool_FromLong(p.isValid()), qPrintable(p.data().toString()),
                         p.isValid() ? db().model->rowCount(p) : -1);
}

PyObject* detach(PyObject*, PyObject*)
{
    auto& s = db();
    s.pins.clear();
    s.tester.reset();
    s.model.reset();
    Py_RETURN_NONE;
}

PyObject* control_warnings(PyObject*, PyObject*)
{
    ensure_app();
    QStringListModel model({ "a", "b" });
    g_warnings = 0;
    QAbstractItemModelTester tester(&model, QAbstractItemModelTester::FailureReportingMode::Warning);
    return PyLong_FromLong(g_warnings);
}

PyMethodDef methods[] = {
    { "attach", attach, METH_VARARGS, "attach(inventory): a DbModel over it, watched by QAbstractItemModelTester" },
    { "process_events", process_events, METH_NOARGS, "runs queued events (model syncs); returns signal counters" },
    { "walk", walk, METH_NOARGS, "walks every index" },
    { "search", search, METH_VARARGS, "search(query) through the model" },
    { "pin", pin, METH_VARARGS, "pin(keys): a persistent index at a key path" },
    { "pinned", pinned, METH_VARARGS, "pinned(i) -> (valid, display, rows)" },
    { "detach", detach, METH_NOARGS, "drops the model" },
    { "control_warnings", control_warnings, METH_NOARGS, "tester warnings on a stock QStringListModel" },
    { nullptr, nullptr, 0, nullptr },
};

PyModuleDef module = { PyModuleDef_HEAD_INIT, "store_qt", nullptr, -1, methods };
} // namespace

PyMODINIT_FUNC PyInit_store_qt()
{
    return PyModule_Create(&module);
}
