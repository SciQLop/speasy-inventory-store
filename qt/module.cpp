// A Python extension holding the Qt side, loaded in the same interpreter as speasy, like SciQLopPlots in SciQLop.
// check_model(store) takes the store through its PyCapsule, then, with the GIL released, runs QAbstractItemModelTester
// on a StoreModel, walks every index, and searches through the model.
#define PY_SSIZE_T_CLEAN
#include <Python.h>

#include "StoreModel.hpp"

#include <QAbstractItemModelTester>
#include <QCoreApplication>
#include <QElapsedTimer>
#include <QStringListModel>

namespace
{
int g_warnings = 0;

void count_warnings(QtMsgType type, const QMessageLogContext&, const QString& message)
{
    if (type != QtDebugMsg && type != QtInfoMsg)
    {
        if (g_warnings++ < 5)
            fprintf(stderr, "model tester: %s\n", qPrintable(message));
    }
}

//! The store's C table, from any Python object exposing __inventory_store_c_api__(); retained by the caller.
bool take_api(PyObject* store, InventoryStoreApi& out)
{
    PyObject* capsule = PyObject_CallMethod(store, "__inventory_store_c_api__", nullptr);
    if (!capsule)
        return false;
    auto* api = static_cast<const InventoryStoreApi*>(PyCapsule_GetPointer(capsule, INVENTORY_STORE_CAPSULE_NAME));
    bool ok = api && api->abi_version == INVENTORY_STORE_ABI_VERSION && api->struct_size >= sizeof(InventoryStoreApi);
    if (ok)
    {
        out = *api;
        out.retain(out.handle); // ours now: the capsule (and even the Python store) may go away
    }
    else if (!PyErr_Occurred())
        PyErr_SetString(PyExc_TypeError, "incompatible inventory store C API");
    Py_DECREF(capsule);
    return ok;
}

struct Walk
{
    long nodes = 0;
    long bad_parents = 0;
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

PyObject* check_model(PyObject*, PyObject* args)
{
    PyObject* store;
    int run_tester = 1;
    const char* query = "mms1 fgm bgse";
    if (!PyArg_ParseTuple(args, "O|ps", &store, &run_tester, &query))
        return nullptr;
    InventoryStoreApi api;
    if (!take_api(store, api))
        return nullptr;

    long nodes, bad_parents, hits, leaf_row_check;
    double tester_ms = 0, walk_ms, search_ms, tooltip_ms;
    Py_BEGIN_ALLOW_THREADS // everything below reads the store without the GIL
    {
        int argc = 0;
        static QCoreApplication* app = QCoreApplication::instance() ? nullptr : new QCoreApplication(argc, nullptr);
        (void)app;
        StoreModel model;
        model.set_store(api);
        api.release(api.handle); // the model holds its own reference

        QElapsedTimer t;
        if (run_tester)
        {
            g_warnings = 0;
            auto previous = qInstallMessageHandler(count_warnings);
            t.start();
            QAbstractItemModelTester tester(&model, QAbstractItemModelTester::FailureReportingMode::Warning);
            tester_ms = t.nsecsElapsed() / 1e6;
            qInstallMessageHandler(previous);
        }
        Walk w;
        t.start();
        walk(model, {}, w);
        walk_ms = t.nsecsElapsed() / 1e6;
        nodes = w.nodes;
        bad_parents = w.bad_parents;

        t.start();
        const auto found = model.search(QString::fromUtf8(query));
        search_ms = t.nsecsElapsed() / 1e6;
        hits = found.size();
        // a hit's index must point back at the same store row
        leaf_row_check = found.isEmpty() ? 1 : model.index_of(found.first().first).internalId() == found.first().first;

        t.start();
        for (int i = 0; i < std::min<long>(found.size(), 1000); ++i)
            (void)model.data(model.index_of(found[i].first), Qt::ToolTipRole);
        tooltip_ms = t.nsecsElapsed() / 1e6;
    }
    Py_END_ALLOW_THREADS

    return Py_BuildValue("{s:l,s:l,s:i,s:d,s:d,s:l,s:d,s:l,s:d}", "nodes", nodes, "bad_parents", bad_parents,
                         "tester_warnings", g_warnings, "tester_ms", tester_ms, "walk_ms", walk_ms, "search_hits",
                         hits, "search_ms", search_ms, "hit_index_ok", leaf_row_check, "tooltips_1000_ms",
                         tooltip_ms);
}

//! The same tester on a stock Core model: warnings it shares with StoreModel come from the tester, not the model.
PyObject* control_warnings(PyObject*, PyObject*)
{
    int argc = 0;
    static QCoreApplication* app = QCoreApplication::instance() ? nullptr : new QCoreApplication(argc, nullptr);
    (void)app;
    QStringListModel model({ "a", "b" });
    g_warnings = 0;
    auto previous = qInstallMessageHandler(count_warnings);
    QAbstractItemModelTester tester(&model, QAbstractItemModelTester::FailureReportingMode::Warning);
    qInstallMessageHandler(previous);
    return PyLong_FromLong(g_warnings);
}

PyMethodDef methods[] = { { "control_warnings", control_warnings, METH_NOARGS, "tester warnings on a QStringListModel" }, { "check_model", check_model, METH_VARARGS,
                            "check_model(store, run_tester=True, query='mms1 fgm bgse') -> dict" },
                          { nullptr, nullptr, 0, nullptr } };

PyModuleDef module = { PyModuleDef_HEAD_INIT, "store_qt", nullptr, -1, methods };
} // namespace

PyMODINIT_FUNC PyInit_store_qt()
{
    return PyModule_Create(&module);
}
