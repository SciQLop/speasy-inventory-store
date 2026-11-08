#include "DbModel.hpp"

#include <QMetaObject>

DbModel::DbModel(const InventoryDbApi* api, QObject* parent) : QAbstractItemModel(parent), m_api(*api)
{
    if (m_api.connect(api, &m_conn) != INV_OK)
        m_conn = 0;
    if (connected() && check(m_api.current_version(m_conn, &m_version)) && check(m_api.pin(m_conn, m_version)))
        check(m_api.subscribe(m_conn, &DbModel::notified, this, &m_token));
}

DbModel::~DbModel()
{
    if (connected())
        m_api.disconnect(m_conn); // drops pins and subscriptions, waits for a notification in progress
}

bool DbModel::check(int32_t status) const
{
    if (status != INV_OK && status != INV_NOT_FOUND)
        ++m_errors;
    return status == INV_OK;
}

void DbModel::notified(void* self)
{
    auto* model = static_cast<DbModel*>(self);
    if (!model->m_sync_queued.exchange(true))
        QMetaObject::invokeMethod(
            model,
            [model]
            {
                model->m_sync_queued = false;
                model->sync();
            },
            Qt::QueuedConnection);
}

void DbModel::move_to(uint64_t version)
{
    if (check(m_api.pin(m_conn, version)))
    {
        check(m_api.unpin(m_conn, m_version));
        m_version = version;
    }
}

void DbModel::sync()
{
    if (m_api.changes(m_conn, m_version, &DbModel::on_change, this) == INV_RESET_NEEDED)
    {
        beginResetModel();
        uint64_t latest = m_version;
        if (check(m_api.current_version(m_conn, &latest)))
            move_to(latest);
        endResetModel();
    }
}

void DbModel::on_change(void* self, const InvDbChange* change)
{
    static_cast<DbModel*>(self)->apply(*change);
}

void DbModel::apply(const InvDbChange& change)
{
    const QModelIndex host = index_of(change.host);
    const int first = static_cast<int>(change.position);
    const int last = static_cast<int>(change.position + change.count) - 1;
    switch (change.kind)
    {
        case INV_CHANGE_INSERTED:
            beginInsertRows(host, first, last);
            move_to(change.version);
            endInsertRows();
            break;
        case INV_CHANGE_REMOVED:
            beginRemoveRows(host, first, last);
            move_to(change.version);
            endRemoveRows();
            break;
        default:
            move_to(change.version);
            if (host.isValid())
                emit dataChanged(host, host);
    }
}

uint64_t DbModel::node_of(const QModelIndex& index) const
{
    return index.isValid() ? static_cast<uint64_t>(index.internalId()) : INVENTORY_DB_ROOT;
}

QString DbModel::text(uint64_t node, uint32_t which) const
{
    QString result;
    check(m_api.text(
        m_conn, m_version, node, which, [](void* ctx, const char* s, size_t n)
        { *static_cast<QString*>(ctx) = QString::fromUtf8(s, static_cast<qsizetype>(n)); },
        &result));
    return result;
}

QModelIndex DbModel::index(int row, int column, const QModelIndex& parent) const
{
    if (!hasIndex(row, column, parent))
        return {};
    uint64_t child = INVENTORY_DB_NO_NODE;
    if (!check(m_api.child(m_conn, m_version, node_of(parent), static_cast<uint32_t>(row), &child)))
        return {};
    return createIndex(row, column, quintptr(child));
}

QModelIndex DbModel::index_of(uint64_t node) const
{
    InvNodeInfo info;
    if (node == INVENTORY_DB_ROOT || !check(m_api.node(m_conn, m_version, node, &info)))
        return {};
    return createIndex(static_cast<int>(info.row), 0, quintptr(node));
}

QModelIndex DbModel::parent(const QModelIndex& index) const
{
    InvNodeInfo info;
    if (!index.isValid() || !check(m_api.node(m_conn, m_version, node_of(index), &info)))
        return {};
    return index_of(info.parent);
}

int DbModel::rowCount(const QModelIndex& parent) const
{
    InvNodeInfo info;
    if (parent.column() > 0 || !connected() || !check(m_api.node(m_conn, m_version, node_of(parent), &info)))
        return 0;
    return static_cast<int>(info.child_count);
}

int DbModel::columnCount(const QModelIndex&) const
{
    return 1;
}

bool DbModel::hasChildren(const QModelIndex& parent) const
{
    return rowCount(parent) > 0;
}

QVariant DbModel::data(const QModelIndex& index, int role) const
{
    if (!index.isValid())
        return {};
    const uint64_t node = node_of(index);
    switch (role)
    {
        case Qt::DisplayRole:
            return text(node, INV_TEXT_DISPLAY);
        case Qt::ToolTipRole:
            return text(node, INV_TEXT_TOOLTIP);
        case KeyRole:
            return text(node, INV_TEXT_KEY);
        case ProviderRole:
            return text(node, INV_TEXT_PROVIDER);
        case UidRole:
            return text(node, INV_TEXT_UID);
        case KindRole:
            return text(node, INV_TEXT_KIND);
        default:
            return {};
    }
}

Qt::ItemFlags DbModel::flags(const QModelIndex& index) const
{
    if (!index.isValid())
        return Qt::NoItemFlags;
    const QString kind = text(node_of(index), INV_TEXT_KIND);
    const bool leaf = kind == QLatin1String("ParameterIndex") || kind == QLatin1String("TemplatedParameterIndex")
        || kind == QLatin1String("ComponentIndex");
    return Qt::ItemIsEnabled | Qt::ItemIsSelectable | (leaf ? Qt::ItemIsDragEnabled : Qt::NoItemFlags);
}

QList<QPair<uint64_t, double>> DbModel::search(const QString& query) const
{
    QList<QPair<uint64_t, double>> hits;
    const QByteArray utf8 = query.toUtf8();
    check(m_api.search(
        m_conn, m_version, utf8.constData(), static_cast<size_t>(utf8.size()),
        [](void* ctx, const uint64_t* nodes, const double* scores, uint32_t n)
        {
            auto* out = static_cast<QList<QPair<uint64_t, double>>*>(ctx);
            out->reserve(n);
            for (uint32_t i = 0; i < n; ++i)
                out->append({ nodes[i], scores[i] });
        },
        &hits));
    return hits;
}
