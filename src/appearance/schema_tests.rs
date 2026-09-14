macro_rules! role_names {
    ($($field:ident),+ $(,)?) => { &[ $(stringify!($field)),+ ] };
}

const CHROME_ROLES: &[&str] = chrome_color_fields!(role_names);
const TERMINAL_ROLES: &[&str] = &[
    "foreground",
    "background",
    "normal",
    "bright",
    "dim",
    "bright_foreground",
    "dim_foreground",
    "cursor",
    "cursor_text",
    "selection_background",
    "selection_foreground",
    "find_match_background",
    "find_match_foreground",
    "find_active_match_background",
    "find_active_match_foreground",
    "hyperlink",
    "visual_bell",
];

#[test]
fn published_schema_lists_exactly_the_accepted_color_roles() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../docs/schema/appearance-settings.schema.json"
    ))
    .expect("published appearance schema must be JSON");
    let listed_chrome: std::collections::BTreeSet<_> =
        schema["$defs"]["chromeColors"]["propertyNames"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|role| role.as_str().unwrap())
            .collect();
    assert_eq!(listed_chrome, CHROME_ROLES.iter().copied().collect());
    let listed_terminal: std::collections::BTreeSet<_> =
        schema["$defs"]["terminalColors"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
    assert_eq!(listed_terminal, TERMINAL_ROLES.iter().copied().collect());
}

#[test]
fn published_schema_and_runtime_preferences_have_identical_fields() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../docs/schema/appearance-settings.schema.json"
    ))
    .expect("published appearance schema must be JSON");
    let runtime = serde_json::to_value(super::AppearanceDocument::default()).unwrap();

    assert_eq!(object_keys(&runtime), property_keys(&schema));
    assert_eq!(
        object_keys(&runtime["preferences"]),
        property_keys(&schema["$defs"]["preferences"])
    );
    assert_eq!(
        object_keys(&runtime["preferences"]["chrome"]),
        property_keys(&schema["$defs"]["preferences"]["properties"]["chrome"])
    );
    assert_eq!(
        object_keys(&runtime["preferences"]["terminal"]),
        property_keys(&schema["$defs"]["preferences"]["properties"]["terminal"])
    );
    assert_eq!(
        object_keys(&runtime["preferences"]["chrome"]["typography"]),
        property_keys(&schema["$defs"]["chromeTypography"])
    );
    assert_eq!(
        object_keys(&runtime["preferences"]["terminal"]["typography"]),
        property_keys(&schema["$defs"]["terminalTypography"])
    );
    assert_eq!(
        object_keys(&runtime["preferences"]["terminal"]["rendering"]),
        property_keys(
            &schema["$defs"]["preferences"]["properties"]["terminal"]["properties"]["rendering"]
        )
    );
}

fn object_keys(value: &serde_json::Value) -> std::collections::BTreeSet<&str> {
    value
        .as_object()
        .expect("runtime value must be an object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn property_keys(value: &serde_json::Value) -> std::collections::BTreeSet<&str> {
    value["properties"]
        .as_object()
        .expect("schema value must declare properties")
        .keys()
        .map(String::as_str)
        .collect()
}
