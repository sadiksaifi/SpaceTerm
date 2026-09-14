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
        "../../docs/schema/color-scheme-definitions-v1.schema.json"
    ))
    .expect("published color-scheme definitions must be JSON");
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
fn published_schemas_resolve_external_draft_2020_12_references() {
    const DEFINITIONS_ID: &str =
        "https://spaceterm.dev/schema/color-scheme-definitions-v1.schema.json";
    let definitions: serde_json::Value = serde_json::from_str(include_str!(
        "../../docs/schema/color-scheme-definitions-v1.schema.json"
    ))
    .expect("published color-scheme definitions must be JSON");
    let settings_schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../docs/schema/appearance-settings.schema.json"
    ))
    .expect("published appearance schema must be JSON");
    let package_schema: serde_json::Value =
        serde_json::from_str(include_str!("../../docs/schema/color-schemes.schema.json"))
            .expect("published color-scheme schema must be JSON");
    assert_eq!(definitions["$id"], DEFINITIONS_ID);

    let settings = serde_json::to_value(super::AppearanceDocument::default()).unwrap();
    let settings_validator = jsonschema::draft202012::options()
        .with_retriever(PublishedSchemaRetriever(definitions.clone()))
        .build(&settings_schema)
        .expect("appearance schema must resolve the shared definitions");
    assert!(settings_validator.is_valid(&settings));
    let mut invalid_settings = settings;
    invalid_settings["preferences"]["chrome"]["schemes"]["light"] = serde_json::json!("INVALID ID");
    assert!(!settings_validator.is_valid(&invalid_settings));

    let package = serde_json::json!({
        "schema_version": 1,
        "schemes": [{
            "kind": "chrome",
            "id": "custom.valid",
            "name": "Valid",
            "appearance": "dark",
            "colors": {}
        }]
    });
    let package_validator = jsonschema::draft202012::options()
        .with_retriever(PublishedSchemaRetriever(definitions))
        .build(&package_schema)
        .expect("color-scheme schema must resolve the shared definitions");
    assert!(package_validator.is_valid(&package));
    assert!(super::parse_color_document(&serde_json::to_vec(&package).unwrap()).is_ok());
    let mut reserved_package = package.clone();
    reserved_package["schemes"][0]["id"] = serde_json::json!("builtin.claimed-by-custom-scheme");
    assert!(!package_validator.is_valid(&reserved_package));
    assert!(super::parse_color_document(&serde_json::to_vec(&reserved_package).unwrap()).is_err());
    let mut invalid_package = package;
    invalid_package["schemes"][0]["appearance"] = serde_json::json!("auto");
    assert!(!package_validator.is_valid(&invalid_package));
}

#[test]
fn published_schema_accepts_sparse_terminal_palettes() {
    let definitions: serde_json::Value = serde_json::from_str(include_str!(
        "../../docs/schema/color-scheme-definitions-v1.schema.json"
    ))
    .unwrap();
    let package_schema: serde_json::Value =
        serde_json::from_str(include_str!("../../docs/schema/color-schemes.schema.json")).unwrap();
    let validator = jsonschema::draft202012::options()
        .with_retriever(PublishedSchemaRetriever(definitions))
        .build(&package_schema)
        .unwrap();
    let package = serde_json::json!({
        "schema_version": 1,
        "schemes": [{
            "kind": "terminal",
            "id": "custom.sparse-ansi",
            "name": "Sparse ANSI",
            "appearance": "dark",
            "colors": {
                "normal": [null, "#dd1133", null, null, null, null, null, null]
            }
        }]
    });

    assert!(validator.is_valid(&package));
    assert!(super::parse_color_document(&serde_json::to_vec(&package).unwrap()).is_ok());
}

struct PublishedSchemaRetriever(serde_json::Value);

impl jsonschema::Retrieve for PublishedSchemaRetriever {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        if uri.as_str().trim_end_matches('#') == self.0["$id"].as_str().unwrap() {
            return Ok(self.0.clone());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "schema URI is not part of the published bundle",
        )
        .into())
    }
}

#[test]
fn published_schema_and_runtime_preferences_have_identical_fields() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../docs/schema/appearance-settings.schema.json"
    ))
    .expect("published appearance schema must be JSON");
    let runtime = serde_json::to_value(super::AppearanceDocument::default()).unwrap();

    assert_eq!(
        runtime["schema_version"],
        schema["properties"]["schema_version"]["const"]
    );
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
        object_keys(&runtime["preferences"]["chrome"]["schemes"]),
        property_keys(&schema["$defs"]["schemeSlots"])
    );
    assert_eq!(
        object_keys(&runtime["preferences"]["terminal"]["schemes"]),
        property_keys(&schema["$defs"]["schemeSlots"])
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
