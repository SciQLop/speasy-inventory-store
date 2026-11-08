// A Qt model over the database-like interface (inventory_db.h): it holds a
// connection number and a pinned version, nothing that points into the inventory.
#pragma once
#include "inventory_db.h"

#include <QAbstractItemModel>
#include <atomic>

static_assert(sizeof(quintptr) >= sizeof(uint64_t), "node ids need a 64-bit quintptr");

class DbModel : public QAbstractItemModel
{
public:
    enum Roles
    {
        KeyRole = Qt::UserRole,
        ProviderRole,
        UidRole,
        KindRole,
    };

    //! Connects (the capsule must be alive during this call), pins the current version, follows changes.
    explicit DbModel(const InventoryDbApi* api, QObject* parent = nullptr);
    ~DbModel() override;
    bool connected() const { return m_conn != 0; }

    QModelIndex index(int row, int column, const QModelIndex& parent = QModelIndex()) const override;
    QModelIndex parent(const QModelIndex& index) const override;
    int rowCount(const QModelIndex& parent = QModelIndex()) const override;
    int columnCount(const QModelIndex& parent = QModelIndex()) const override;
    bool hasChildren(const QModelIndex& parent = QModelIndex()) const override;
    QVariant data(const QModelIndex& index, int role = Qt::DisplayRole) const override;
    Qt::ItemFlags flags(const QModelIndex& index) const override;

    QModelIndex index_of(uint64_t node) const;
    QList<QPair<uint64_t, double>> search(const QString& query) const;
    void sync();
    //! Calls that returned a status other than INV_OK / INV_NOT_FOUND (should stay 0).
    long errors() const { return m_errors; }

private:
    static void notified(void* self);
    static void on_change(void* self, const InvDbChange* change);
    void apply(const InvDbChange& change);
    void move_to(uint64_t version);
    uint64_t node_of(const QModelIndex& index) const;
    QString text(uint64_t node, uint32_t which) const;
    bool check(int32_t status) const;

    InventoryDbApi m_api;
    uint64_t m_conn = 0;
    uint64_t m_version = 0;
    uint64_t m_token = 0;
    mutable long m_errors = 0;
    std::atomic<bool> m_sync_queued { false };
};
