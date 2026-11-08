// A Qt item model over speasy's inventory, through its C interface (include/inventory_store.h).
// No node objects: every index carries the inventory node id (QModelIndex::internalId).
// The model reads one immutable snapshot, so it never locks or takes the GIL. When the inventory grows (lazy
// providers, lazy nodes) or changes (refresh), it replays each change on its own thread: begin signal, switch to the
// snapshot after that change, end signal. Unchanged parts keep their indexes, expansion and selection.
#pragma once
#include "inventory_store.h"

#include <QAbstractItemModel>
#include <atomic>

static_assert(sizeof(quintptr) >= sizeof(InvNode), "node ids need a 64-bit quintptr");

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

    //! Retains the inventory, takes its current snapshot and follows its changes.
    explicit StoreModel(const InventoryApi& api, QObject* parent = nullptr);
    ~StoreModel() override;

    QModelIndex index(int row, int column, const QModelIndex& parent = QModelIndex()) const override;
    QModelIndex parent(const QModelIndex& index) const override;
    int rowCount(const QModelIndex& parent = QModelIndex()) const override;
    int columnCount(const QModelIndex& parent = QModelIndex()) const override;
    bool hasChildren(const QModelIndex& parent = QModelIndex()) const override;
    QVariant data(const QModelIndex& index, int role = Qt::DisplayRole) const override;
    Qt::ItemFlags flags(const QModelIndex& index) const override;

    //! Index of a node (e.g. a search hit), for selecting or expanding it in a view.
    QModelIndex index_of(InvNode node) const;
    //! SciQLop's product search over the model's snapshot: nodes of the hits with their score.
    QList<QPair<InvNode, double>> search(const QString& query) const;
    //! Replays the inventory's changes since the last sync. Runs on the model's thread; queued by the notification.
    void sync();

private:
    static void notified(void* self);
    static void replay(void* self, const InvChange* change, InvSnapshot after);
    void apply(const InvChange& change, InvSnapshot after);
    void switch_to(InvSnapshot snapshot, uint64_t seq);
    InvNode node_of(const QModelIndex& index) const;
    QString text(InvStr s) const;
    QString display_name(InvNode node) const;
    QString tooltip(InvNode node) const;

    InventoryApi m_api;
    InvSnapshot m_snap = nullptr;
    uint64_t m_seq = 0;
    uint64_t m_token = 0;
    std::atomic<bool> m_sync_queued { false };
};
