#include "StoreModel.hpp"

namespace
{
constexpr uint32_t ROOT = 0;

InvStr inv(const QByteArray& utf8)
{
    return { utf8.constData(), static_cast<size_t>(utf8.size()) };
}
} // namespace

StoreModel::StoreModel(QObject* parent) : QAbstractItemModel(parent) { }

StoreModel::~StoreModel()
{
    if (m_has_store)
        m_api.release(m_api.handle);
}

void StoreModel::set_store(const InventoryStoreApi& api)
{
    beginResetModel();
    api.retain(api.handle);
    if (m_has_store)
        m_api.release(m_api.handle);
    m_api = api;
    m_has_store = true;
    endResetModel();
}

uint32_t StoreModel::row_of(const QModelIndex& index) const
{
    return index.isValid() ? static_cast<uint32_t>(index.internalId()) : ROOT;
}

QString StoreModel::text(InvStr s) const
{
    return QString::fromUtf8(s.ptr, static_cast<qsizetype>(s.len));
}

QModelIndex StoreModel::index(int row, int column, const QModelIndex& parent) const
{
    if (!m_has_store || !hasIndex(row, column, parent))
        return {};
    const uint32_t child = m_api.child(m_api.handle, row_of(parent), static_cast<uint32_t>(row));
    return child == INVENTORY_STORE_NONE ? QModelIndex() : createIndex(row, column, quintptr(child));
}

QModelIndex StoreModel::index_of(uint32_t row) const
{
    if (!m_has_store || row == ROOT || row >= m_api.node_count(m_api.handle))
        return {};
    return createIndex(static_cast<int>(m_api.row_in_parent(m_api.handle, row)), 0, quintptr(row));
}

QModelIndex StoreModel::parent(const QModelIndex& index) const
{
    if (!m_has_store || !index.isValid())
        return {};
    const uint32_t p = m_api.parent(m_api.handle, row_of(index));
    return (p == INVENTORY_STORE_NONE || p == ROOT) ? QModelIndex() : index_of(p);
}

int StoreModel::rowCount(const QModelIndex& parent) const
{
    if (!m_has_store || parent.column() > 0)
        return 0;
    return static_cast<int>(m_api.child_count(m_api.handle, row_of(parent)));
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
QString StoreModel::display_name(uint32_t row) const
{
    const auto h = m_api.handle;
    const uint32_t i = m_api.meta_find(h, row, inv(QByteArrayLiteral("name")));
    if (i != INVENTORY_STORE_NONE)
    {
        InvValue v;
        m_api.meta_value(h, row, i, &v);
        if (v.kind == INV_STR && QByteArrayView(v.text.ptr, qsizetype(v.text.len)) != "AMDA")
            return text(v.text);
    }
    return text(m_api.field(h, row, INV_FIELD_KEY));
}

// Built on demand, like SciQLop's precomputed one: "<h3>name</h3><br/><b>key:</b> value" per metadata entry.
QString StoreModel::tooltip(uint32_t row) const
{
    const auto h = m_api.handle;
    QString html = QStringLiteral("<h3>%1</h3>").arg(display_name(row));
    const uint32_t n = m_api.meta_count(h, row);
    for (uint32_t i = 0; i < n; ++i)
    {
        QString value;
        m_api.meta_text(
            h, row, i, [](void* ctx, InvStr s)
            { *static_cast<QString*>(ctx) = QString::fromUtf8(s.ptr, qsizetype(s.len)); },
            &value);
        html += QStringLiteral("<br/><b>%1:</b> %2").arg(text(m_api.meta_key(h, row, i)), value);
    }
    return html;
}

QVariant StoreModel::data(const QModelIndex& index, int role) const
{
    if (!m_has_store || !index.isValid())
        return {};
    const uint32_t row = row_of(index);
    switch (role)
    {
        case Qt::DisplayRole:
            return display_name(row);
        case Qt::ToolTipRole:
            return tooltip(row);
        case KeyRole:
            return text(m_api.field(m_api.handle, row, INV_FIELD_KEY));
        case ProviderRole:
            return text(m_api.field(m_api.handle, row, INV_FIELD_PROVIDER));
        case UidRole:
            return text(m_api.field(m_api.handle, row, INV_FIELD_UID));
        case KindRole:
            return text(m_api.field(m_api.handle, row, INV_FIELD_KIND));
        default:
            return {};
    }
}

Qt::ItemFlags StoreModel::flags(const QModelIndex& index) const
{
    if (!index.isValid())
        return Qt::NoItemFlags;
    const bool leaf = m_api.child_count(m_api.handle, row_of(index)) == 0;
    return Qt::ItemIsEnabled | Qt::ItemIsSelectable
        | (leaf ? Qt::ItemIsDragEnabled | Qt::ItemNeverHasChildren : Qt::NoItemFlags);
}

QList<QPair<uint32_t, double>> StoreModel::search(const QString& query) const
{
    QList<QPair<uint32_t, double>> hits;
    if (!m_has_store)
        return hits;
    const QByteArray utf8 = query.toUtf8();
    m_api.search(
        m_api.handle, inv(utf8), [](void* ctx, uint32_t row, double score)
        { static_cast<QList<QPair<uint32_t, double>>*>(ctx)->append({ row, score }); },
        &hits);
    return hits;
}
