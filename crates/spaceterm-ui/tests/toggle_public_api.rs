use gpui::{ElementId, Window};
use spaceterm_ui::{Checkbox, CheckboxState, Switch, ToggleActivationSource, ToggleSize};

const _: fn() = || {
    let checkbox = Checkbox::new(
        ElementId::Name("public-checkbox".into()),
        "Restore panes when SpaceTerm opens",
        CheckboxState::Mixed,
    )
    .size(ToggleSize::Compact)
    .disabled(false)
    .tab_stop(true)
    .label_hidden(false)
    .full_width(true)
    .right_to_left(false)
    .debug_selector("public-checkbox")
    .on_change(|change, _: &mut Window, _| {
        let _: CheckboxState = change.previous();
        let _: CheckboxState = change.requested();
        let _: ToggleActivationSource = change.source();
    });
    let switch = Switch::new("public-switch", "Attention notifications", false)
        .size(ToggleSize::Regular)
        .disabled(false)
        .tab_stop(true)
        .label_hidden(false)
        .full_width(true)
        .right_to_left(false)
        .debug_selector("public-switch")
        .on_change(|change, _: &mut Window, _| {
            let _: bool = change.previous();
            let _: bool = change.requested();
            let _: ToggleActivationSource = change.source();
        });

    let _ = (checkbox, switch);
};
