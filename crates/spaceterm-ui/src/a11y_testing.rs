//! Reads a test window's accessibility tree the way assistive technology receives it.

use gpui::{VisualTestContext, accesskit};
use serde_json::Value;

/// The last accessibility tree a test window published.
pub(crate) struct A11yTree(Value);

impl A11yTree {
    /// Activates accessibility on first use and returns the current tree.
    pub(crate) fn read(cx: &mut VisualTestContext) -> Self {
        let active = cx.update(|window, _| window.is_a11y_active());
        if !active {
            cx.activate_accessibility();
        }
        cx.run_until_parked();
        Self(cx.update(|window, _| {
            serde_json::from_str(&window.debug_a11y_tree_json().expect("a published tree"))
                .expect("a valid tree")
        }))
    }

    /// Returns the node with this accessible name, if one is published.
    pub(crate) fn find(&self, label: &str) -> Option<&Value> {
        self.nodes().find(|node| node["aria"]["label"] == label)
    }

    /// Returns the node with this accessible name.
    pub(crate) fn node(&self, label: &str) -> &Value {
        self.find(label)
            .unwrap_or_else(|| panic!("no node is named {label:?}"))
    }

    /// Returns every published node with this role, in tree order.
    pub(crate) fn with_role(&self, role: &str) -> Vec<&Value> {
        self.nodes()
            .filter(|node| node["aria"]["role"] == role)
            .collect()
    }

    /// Returns the node GPUI reports as focused, preferring an active descendant.
    pub(crate) fn focused(&self) -> Option<&Value> {
        let key = self.0["active_descendant_focus"]
            .as_str()
            .or_else(|| self.0["gpui_focus"].as_str())?;
        self.0["nodes"].get(key)
    }

    /// Returns a node's children in tree order.
    pub(crate) fn children(&self, node: &Value) -> Vec<&Value> {
        node["children"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|key| self.0["nodes"].get(key.as_str()?))
            .collect()
    }

    fn nodes(&self) -> impl Iterator<Item = &Value> {
        self.0["nodes"]
            .as_object()
            .into_iter()
            .flat_map(|nodes| nodes.values())
    }
}

/// Reports whether a node advertises this action.
pub(crate) fn supports(node: &Value, action: accesskit::Action) -> bool {
    node["aria"]["on_action"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|name| *name == format!("{action:?}")))
}

/// Sends an assistive technology action request to a published node.
pub(crate) fn perform(cx: &mut VisualTestContext, node: &Value, action: accesskit::Action) {
    perform_with(cx, node, action, None);
}

/// Sends an assistive technology action request with data to a published node.
pub(crate) fn perform_with(
    cx: &mut VisualTestContext,
    node: &Value,
    action: accesskit::Action,
    data: Option<accesskit::ActionData>,
) {
    let target_node = node_id(node);
    cx.simulate_accessibility_action(accesskit::ActionRequest {
        action,
        target_tree: accesskit::TreeId::ROOT,
        target_node,
        data,
    });
}

/// Returns a published node's AccessKit id.
pub(crate) fn node_id(node: &Value) -> accesskit::NodeId {
    accesskit::NodeId(
        node["accesskit_id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .expect("a published node id"),
    )
}
