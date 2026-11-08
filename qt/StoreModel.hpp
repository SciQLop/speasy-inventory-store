// A Qt item model reading speasy's inventory store through its C interface: no node objects, every index carries
// the store row id (QModelIndex::internalId). The store is immutable, so the model never locks or takes the GIL.
#pragma once
#include "inventory_store.h"

#include <QAbstractItemModel>

class StoreModel : public QAbstractItemModel
{
public:
    enum Roles
    {
        KeyRole = Qt::UserRole, // SciQLop's identity role: the attribute name used by path lookup
        ProviderRole,
        UidRole,
        KindRole,
    };

    explicit StoreModel(QObject* parent = nullptr);
    ~StoreModel() override;

    //! Takes its own reference on the store (retain); a refreshed inventory is a new store: one model reset.
    void set_store(const InventoryStoreApi& api);

    QModelIndex index(int row, int column, const QModelIndex& parent = QModelIndex()) const override;
    QModelIndex parent(const QModelIndex& index) const override;
    int rowCount(const QModelIndex& parent = QModelIndex()) const override;
    int columnCount(const QModelIndex& parent = QModelIndex()) const override;
    bool hasChildren(const QModelIndex& parent = QModelIndex()) const override;
    QVariant data(const QModelIndex& index, int role = Qt::DisplayRole) const override;
    Qt::ItemFlags flags(const QModelIndex& index) const override;

    //! Index of a store row (e.g. a search hit), for selecting or expanding it in a view.
    QModelIndex index_of(uint32_t row) const;
    //! SciQLop's product search over the store: rows of the hits with their score.
    QList<QPair<uint32_t, double>> search(const QString& query) const;

private:
    uint32_t row_of(const QModelIndex& index) const;
    QString text(InvStr s) const;
    QString display_name(uint32_t row) const;
    QString tooltip(uint32_t row) const;

    InventoryStoreApi m_api {};
    bool m_has_store = false;
};
