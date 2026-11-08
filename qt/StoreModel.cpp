#include "StoreModel.hpp"

#include <QMetaObject>

namespace
{
InvStr inv(const QByteArray& utf8)
{
    return { utf8.constData(), static_cast<size_t>(utf8.size()) };
}
} // namespace

StoreModel::StoreModel(const InventoryApi& api, QObject* parent) : QAbstractItemModel(parent), m_api(api)
{
    m_api.retain(m_api.handle);
    m_snap = m_api.snapshot(m_api.handle);
    m_seq = m_api.snapshot_seq(m_snap);
    m_token = m_api.subscribe(m_api.handle, &StoreModel::notified, this);
}

StoreModel::~StoreModel()
{
    m_api.unsubscribe(m_api.handle, m_token); // waits for a notification in progress
    m_api.snapshot_release(m_snap);
    m_api.release(m_api.handle);
}

// On the thread that changed the inventory: only queue one sync on the model's thread. Qt drops it if the model is
// destroyed before it runs.
void StoreModel::notified(void* self)
{
    auto* model = static_cast<StoreModel*>(self);
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

void StoreModel::sync()
{
    if (m_api.changes_since(m_api.handle, m_seq, &StoreModel::replay, this) == INVENTORY_NONE)
    {
        beginResetModel(); // fell behind the change log
        InvSnapshot latest = m_api.snapshot(m_api.handle);
        switch_to(latest, m_api.snapshot_seq(latest));
        m_api.snapshot_release(latest);
        endResetModel();
    }
}

void StoreModel::replay(void* self, const InvChange* change, InvSnapshot after)
{
    static_cast<StoreModel*>(self)->apply(*change, after);
}

void StoreModel::switch_to(InvSnapshot snapshot, uint64_t seq)
{
    m_api.snapshot_retain(snapshot);
    m_api.snapshot_release(m_snap);
    m_snap = snapshot;
    m_seq = seq;
}

// begin* reads the snapshot before the change (where the host's index is valid), end* the one after.
void StoreModel::apply(const InvChange& change, InvSnapshot after)
{
    const QModelIndex host = index_of(change.host);
    const int first = static_cast<int>(change.position);
    const int last = static_cast<int>(change.position + change.count) - 1;
    switch (change.kind)
    {
        case INV_CHANGE_INSERTED:
            beginInsertRows(host, first, last);
            switch_to(after, change.seq);
            endInsertRows();
            break;
        case INV_CHANGE_REMOVED:
            beginRemoveRows(host, first, last);
            switch_to(after, change.seq);
            endRemoveRows();
            break;
        default:
            switch_to(after, change.seq);
            if (host.isValid())
                emit dataChanged(host, host);
    }
}

InvNode StoreModel::node_of(const QModelIndex& index) const
{
    return index.isValid() ? static_cast<InvNode>(index.internalId()) : INVENTORY_ROOT;
}

QString StoreModel::text(InvStr s) const
{
    return QString::fromUtf8(s.ptr, static_cast<qsizetype>(s.len));
}

QModelIndex StoreModel::index(int row, int column, const QModelIndex& parent) const
{
    if (!hasIndex(row, column, parent))
        return {};
    const InvNode child = m_api.child(m_snap, node_of(parent), static_cast<uint32_t>(row));
    return child == INVENTORY_NO_NODE ? QModelIndex() : createIndex(row, column, quintptr(child));
}

QModelIndex StoreModel::index_of(InvNode node) const
{
    if (node == INVENTORY_ROOT || node == INVENTORY_NO_NODE)
        return {};
    const uint32_t row = m_api.row_in_parent(m_snap, node);
    return row == INVENTORY_NONE ? QModelIndex() : createIndex(static_cast<int>(row), 0, quintptr(node));
}

QModelIndex StoreModel::parent(const QModelIndex& index) const
{
    if (!index.isValid())
        return {};
    return index_of(m_api.parent(m_snap, node_of(index)));
}

int StoreModel::rowCount(const QModelIndex& parent) const
{
    if (parent.column() > 0)
        return 0;
    return static_cast<int>(m_api.child_count(m_snap, node_of(parent)));
}

int StoreModel::columnCount(const QModelIndex&) const
{
    return 1;
}

bool StoreModel::hasChildren(const QModelIndex& parent) const
{
    return rowCount(parent) > 0;
}

// SciQLop's speasy plugin shows a node's "name" metadata when it has one (except "AMDA"), else its attribute key.
QString StoreModel::display_name(InvNode node) const
{
    const uint32_t i = m_api.meta_find(m_snap, node, inv(QByteArrayLiteral("name")));
    if (i != INVENTORY_NONE)
    {
        InvValue v;
        m_api.meta_value(m_snap, node, i, &v);
        if (v.kind == INV_STR && QByteArrayView(v.text.ptr, qsizetype(v.text.len)) != "AMDA")
            return text(v.text);
    }
    return text(m_api.field(m_snap, node, INV_FIELD_KEY));
}

QString StoreModel::tooltip(InvNode node) const
{
    QString html = QStringLiteral("<h3>%1</h3>").arg(display_name(node));
    const uint32_t n = m_api.meta_count(m_snap, node);
    for (uint32_t i = 0; i < n; ++i)
    {
        QString value;
        m_api.meta_text(
            m_snap, node, i, [](void* ctx, InvStr s)
            { *static_cast<QString*>(ctx) = QString::fromUtf8(s.ptr, qsizetype(s.len)); },
            &value);
        html += QStringLiteral("<br/><b>%1:</b> %2").arg(text(m_api.meta_key(m_snap, node, i)), value);
    }
    return html;
}

QVariant StoreModel::data(const QModelIndex& index, int role) const
{
    if (!index.isValid())
        return {};
    const InvNode node = node_of(index);
    switch (role)
    {
        case Qt::DisplayRole:
            return display_name(node);
        case Qt::ToolTipRole:
            return tooltip(node);
        case KeyRole:
            return text(m_api.field(m_snap, node, INV_FIELD_KEY));
        case ProviderRole:
            return text(m_api.field(m_snap, node, INV_FIELD_PROVIDER));
        case UidRole:
            return text(m_api.field(m_snap, node, INV_FIELD_UID));
        case KindRole:
            return text(m_api.field(m_snap, node, INV_FIELD_KIND));
        default:
            return {};
    }
}

// Leaves are flagged by kind, not by having no children: a lazy node has none until it fills in.
Qt::ItemFlags StoreModel::flags(const QModelIndex& index) const
{
    if (!index.isValid())
        return Qt::NoItemFlags;
    const QString kind = text(m_api.field(m_snap, node_of(index), INV_FIELD_KIND));
    const bool leaf = kind == QLatin1String("ParameterIndex") || kind == QLatin1String("TemplatedParameterIndex")
        || kind == QLatin1String("ComponentIndex");
    return Qt::ItemIsEnabled | Qt::ItemIsSelectable | (leaf ? Qt::ItemIsDragEnabled : Qt::NoItemFlags);
}

QList<QPair<InvNode, double>> StoreModel::search(const QString& query) const
{
    QList<QPair<InvNode, double>> hits;
    const QByteArray utf8 = query.toUtf8();
    m_api.search(
        m_snap, inv(utf8), [](void* ctx, InvNode node, double score)
        { static_cast<QList<QPair<InvNode, double>>*>(ctx)->append({ node, score }); },
        &hits);
    return hits;
}
