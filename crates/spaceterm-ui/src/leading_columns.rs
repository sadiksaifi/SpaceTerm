use gpui::{
    AnyElement, Div, ParentElement as _, Pixels, Styled as _, div, prelude::FluentBuilder as _, px,
};

/// Widths of the leading columns and the gap between them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LeadingColumnMetrics {
    pub(crate) state_width: Pixels,
    pub(crate) icon_width: Pixels,
    pub(crate) column_gap: Pixels,
}

/// The leading columns one group of list rows reserves before its labels.
///
/// The state column carries the checkmark of a row whose state the group can show. The icon
/// column carries a row's own symbol. A group reserves a column only when one of its rows uses
/// it, so every label in the group starts at one edge and a command never sits behind an empty
/// checkmark column.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LeadingColumns {
    state: bool,
    icon: bool,
}

impl LeadingColumns {
    /// Reserves the state column when a row in the group can show state and the icon column when
    /// a row in the group has an icon.
    pub(crate) fn new(state: bool, icon: bool) -> Self {
        Self { state, icon }
    }

    /// Returns the distance from a row's content edge to its label.
    pub(crate) fn label_offset(self, metrics: LeadingColumnMetrics, label_gap: Pixels) -> Pixels {
        let columns = match (self.state, self.icon) {
            (false, false) => return px(0.0),
            (true, false) => metrics.state_width,
            (false, true) => metrics.icon_width,
            (true, true) => metrics.state_width + metrics.column_gap + metrics.icon_width,
        };
        columns + label_gap
    }

    /// Lays out one row's reserved columns, or nothing when the group reserves none.
    ///
    /// The mark fills the state column and the icon fills the icon column. Content for a column
    /// the group does not reserve is not drawn.
    pub(crate) fn render(
        self,
        metrics: LeadingColumnMetrics,
        mark: Option<AnyElement>,
        icon: Option<AnyElement>,
    ) -> Option<Div> {
        if !self.state && !self.icon {
            return None;
        }
        let column = |width: Pixels, content: Option<AnyElement>| {
            div()
                .w(width)
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .children(content)
        };
        Some(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(metrics.column_gap)
                .when(self.state, |columns| {
                    columns.child(column(metrics.state_width, mark))
                })
                .when(self.icon, |columns| {
                    columns.child(column(metrics.icon_width, icon))
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_offset_should_count_only_reserved_columns() {
        let metrics = LeadingColumnMetrics {
            state_width: px(16.0),
            icon_width: px(18.0),
            column_gap: px(4.0),
        };
        let gap = px(6.0);

        assert_eq!(
            LeadingColumns::new(false, false).label_offset(metrics, gap),
            px(0.0)
        );
        assert_eq!(
            LeadingColumns::new(true, false).label_offset(metrics, gap),
            px(22.0)
        );
        assert_eq!(
            LeadingColumns::new(false, true).label_offset(metrics, gap),
            px(24.0)
        );
        assert_eq!(
            LeadingColumns::new(true, true).label_offset(metrics, gap),
            px(44.0)
        );
    }
}
