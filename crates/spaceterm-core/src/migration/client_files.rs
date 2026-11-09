//! The records SpaceTerm Core's client library keeps for each client under `clients/`: the
//! files the CLI's and the desktop's state roots share with this library.
#[cfg(feature = "client")]
use super::{DataKind, FileRule};

/// One [`FileRule`] for each record a client directory holds.
#[cfg(feature = "client")]
pub const RULES: &[FileRule] = &[
    FileRule {
        name: "client.json",
        kind: DataKind::ClientRecords,
        read: crate::client::readable_identity,
        stamp: None,
    },
    FileRule {
        name: "route.json",
        kind: DataKind::ClientRecords,
        read: crate::client::readable_route,
        stamp: None,
    },
    FileRule {
        name: "operations.json",
        kind: DataKind::ClientRecords,
        read: crate::hierarchy::readable_journal,
        stamp: None,
    },
    FileRule {
        name: "service-operations.json",
        kind: DataKind::ClientRecords,
        read: crate::service::lane::admin::readable,
        stamp: None,
    },
    FileRule {
        name: "attachments.json",
        kind: DataKind::View,
        read: crate::attachments::readable,
        stamp: None,
    },
    FileRule {
        name: "view.json",
        kind: DataKind::View,
        read: crate::client_view::readable,
        stamp: None,
    },
    FileRule {
        name: "inventory.json",
        kind: DataKind::View,
        read: crate::client_view::readable_inventory,
        stamp: None,
    },
    FileRule {
        name: "pane-restarts.json",
        kind: DataKind::View,
        read: crate::client_view::readable_pane_restarts,
        stamp: None,
    },
    FileRule {
        name: "workspace-order.json",
        kind: DataKind::WorkspaceOrder,
        read: crate::workspace_order::readable,
        stamp: None,
    },
];
