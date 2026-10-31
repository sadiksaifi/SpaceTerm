fn published_schema() -> serde_json::Value {
    serde_json::from_str(include_str!("../../docs/schema/settings.schema.json"))
        .expect("published settings schema must be JSON")
}

fn validator() -> jsonschema::Validator {
    jsonschema::draft202012::new(&published_schema()).expect("published schema must compile")
}

#[test]
fn published_schema_and_parser_accept_color_values_and_reject_unknown_or_mistyped_roles() {
    let validator = validator();
    let mut settings = serde_json::to_value(super::SettingsDocument::default()).unwrap();
    // A complete authored palette exercises the schema and parser, including array roles.
    let colors = serde_json::json!({
        "foreground": "#abcdefff", "background": "#123456ff",
        "normal": ["#111111ff", "#222222ff", "#333333ff", "#444444ff", "#555555ff", "#666666ff", "#777777ff", "#888888ff"],
        "bright": ["#111111ff", "#222222ff", "#333333ff", "#444444ff", "#555555ff", "#666666ff", "#777777ff", "#888888ff"],
        "dim": ["#111111ff", "#222222ff", "#333333ff", "#444444ff", "#555555ff", "#666666ff", "#777777ff", "#888888ff"],
        "bright_foreground": "#fedcbaff", "dim_foreground": "#654321ff",
        "cursor": "#aabbccff", "cursor_text": "#001122ff",
        "selection_background": "#12345680", "selection_foreground": "#abcdefff",
        "find_match_background": "#23456780", "find_match_foreground": "#bcdef0ff",
        "find_active_match_background": "#34567880", "find_active_match_foreground": "#cdef01ff",
        "hyperlink": "#456789ff", "visual_bell": "#56789a80"
    });
    settings["appearance"]["terminal"]["overrides"] = serde_json::json!({
        "builtin.spaceterm.dark": colors
    });
    let parse =
        |value: &serde_json::Value| super::parse_settings(&serde_json::to_vec(value).unwrap());
    assert!(validator.is_valid(&settings));
    let parsed = parse(&settings).unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["appearance"]["terminal"]["overrides"]["builtin.spaceterm.dark"],
        colors
    );
    for (role, invalid) in [
        ("foreground", serde_json::json!("#xyzxyz")),
        ("foreground", serde_json::json!(["#abcdefff"])),
        ("normal", serde_json::json!("#abcdefff")),
        ("bright", serde_json::json!(["#abcdefff"])),
        ("unknown_role", serde_json::json!("#abcdefff")),
        ("terminal.foreground", serde_json::json!("#abcdefff")),
        ("Foreground", serde_json::json!("#abcdefff")),
    ] {
        let mut invalid_settings = settings.clone();
        invalid_settings["appearance"]["terminal"]["overrides"]["builtin.spaceterm.dark"][role] =
            invalid;
        assert!(!validator.is_valid(&invalid_settings), "{role}");
        assert!(parse(&invalid_settings).is_err(), "{role}");
    }
}

#[test]
fn published_schema_accepts_defaults_and_installed_themes_and_rejects_reserved_ids() {
    let validator = validator();
    let settings = serde_json::to_value(super::SettingsDocument::default()).unwrap();
    assert!(validator.is_valid(&settings));
    let mut invalid_settings = settings.clone();
    invalid_settings["appearance"]["terminal"]["themes"]["light"] = serde_json::json!("INVALID ID");
    assert!(!validator.is_valid(&invalid_settings));

    let family = br##"{"name":"Sample","themes":[{"name":"Sample Dark","appearance":"dark","style":{"terminal.foreground":"#abcdef"}}]}"##;
    let mut installed = settings;
    installed["terminal_themes"] =
        serde_json::to_value(crate::appearance::translate_zed_family(family).unwrap()).unwrap();
    assert!(validator.is_valid(&installed));
    assert!(super::parse_settings(&serde_json::to_vec(&installed).unwrap()).is_ok());

    let mut reserved = installed.clone();
    reserved["terminal_themes"][0]["id"] = serde_json::json!("builtin.claimed-by-custom-theme");
    assert!(!validator.is_valid(&reserved));
    assert!(super::parse_settings(&serde_json::to_vec(&reserved).unwrap()).is_err());

    let mut unknown_role = installed;
    unknown_role["terminal_themes"][0]["colors"]["unknown_role"] = serde_json::json!("#ffffff");
    assert!(!validator.is_valid(&unknown_role));
    assert!(super::parse_settings(&serde_json::to_vec(&unknown_role).unwrap()).is_err());
}

#[test]
fn published_schema_accepts_sparse_terminal_palettes() {
    let mut settings = serde_json::to_value(super::SettingsDocument::default()).unwrap();
    settings["appearance"]["terminal"]["overrides"] = serde_json::json!({
        "builtin.spaceterm.dark": {
            "normal": [null, "#dd1133", null, null, null, null, null, null]
        }
    });

    assert!(validator().is_valid(&settings));
    assert!(super::parse_settings(&serde_json::to_vec(&settings).unwrap()).is_ok());
}

#[test]
fn named_font_families_have_matching_schema_and_runtime_character_rules() {
    let validator = validator();

    let mut invalid = serde_json::to_value(super::SettingsDocument::default()).unwrap();
    invalid["appearance"]["terminal"]["typography"]["family"] =
        serde_json::json!({"source": "named", "family": "Broken\nFamily"});
    assert!(!validator.is_valid(&invalid));
    assert!(super::parse_settings(&serde_json::to_vec(&invalid).unwrap()).is_err());

    let mut unicode = serde_json::to_value(super::SettingsDocument::default()).unwrap();
    unicode["appearance"]["terminal"]["typography"]["family"] =
        serde_json::json!({"source": "named", "family": "ヒラギノ角ゴシック"});
    assert!(validator.is_valid(&unicode));
    assert!(super::parse_settings(&serde_json::to_vec(&unicode).unwrap()).is_ok());
}

#[test]
fn published_schema_and_runtime_preferences_have_identical_fields() {
    let schema = published_schema();
    let runtime = serde_json::to_value(super::SettingsDocument::default()).unwrap();

    assert_eq!(
        runtime["schema_version"],
        schema["properties"]["schema_version"]["const"]
    );
    assert_eq!(object_keys(&runtime), property_keys(&schema));
    assert_eq!(
        object_keys(&runtime["clipboard"]),
        property_keys(&schema["$defs"]["clipboard"])
    );
    assert_eq!(
        object_keys(&runtime["git"]),
        property_keys(&schema["$defs"]["git"])
    );
    assert_eq!(
        object_keys(&runtime["sidebar"]),
        property_keys(&schema["$defs"]["sidebar"])
    );
    assert_eq!(
        object_keys(&runtime["updates"]),
        property_keys(&schema["$defs"]["updates"])
    );
    assert_eq!(
        object_keys(&runtime["appearance"]),
        property_keys(&schema["$defs"]["appearancePreferences"])
    );
    assert_eq!(
        object_keys(&runtime["appearance"]["terminal"]),
        property_keys(&schema["$defs"]["appearancePreferences"]["properties"]["terminal"])
    );
    assert_eq!(
        object_keys(&runtime["appearance"]["window"]),
        property_keys(&schema["$defs"]["window"])
    );
    assert_eq!(
        object_keys(&runtime["appearance"]["terminal"]["themes"]),
        property_keys(&schema["$defs"]["themeSlots"])
    );
    assert_eq!(
        object_keys(&runtime["appearance"]["terminal"]["typography"]),
        property_keys(&schema["$defs"]["terminalTypography"])
    );
    assert_eq!(
        object_keys(&runtime["appearance"]["terminal"]["rendering"]),
        property_keys(
            &schema["$defs"]["appearancePreferences"]["properties"]["terminal"]["properties"]["rendering"]
        )
    );
}

#[test]
fn published_schema_lists_keybinding_commands_in_settings_order() {
    let schema = published_schema();
    let expected: Vec<_> = crate::keybindings::Command::ALL
        .iter()
        .map(|command| command.id())
        .collect();
    assert_eq!(
        property_keys(&schema["$defs"]["keybindings"]),
        expected.iter().copied().collect()
    );

    let source = include_str!("../../docs/schema/settings.schema.json");
    let positions: Vec<_> = expected
        .iter()
        .map(|id| {
            source
                .find(&format!("\"{id}\":"))
                .expect("schema must list every command id")
        })
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));

    let validator = validator();
    let mut settings = serde_json::to_value(super::SettingsDocument::default()).unwrap();
    settings["keybindings"] =
        serde_json::json!({"close_tab": null, "new_workspace": "shift-cmd-t"});
    assert!(validator.is_valid(&settings));
    settings["keybindings"]["unknown_command"] = serde_json::json!(null);
    assert!(!validator.is_valid(&settings));
    settings["keybindings"]
        .as_object_mut()
        .unwrap()
        .remove("unknown_command");
    settings["keybindings"]["close_tab"] = serde_json::json!(true);
    assert!(!validator.is_valid(&settings));
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

#[test]
fn published_schema_and_parser_agree_on_the_background_image_digest() {
    let validator = validator();
    let mut settings = serde_json::to_value(super::SettingsDocument::default()).unwrap();
    assert!(settings["appearance"]["window"]["background_image"].is_null());

    settings["appearance"]["window"]["background_image"] = serde_json::json!("0f".repeat(32));
    assert!(validator.is_valid(&settings));
    let document = super::parse_settings(&serde_json::to_vec(&settings).unwrap()).unwrap();
    assert!(document.appearance.window.background_image.is_some());

    for invalid in [
        "0F".repeat(32),
        "0f".repeat(31),
        String::from("/Users/me/photo.jpg"),
    ] {
        settings["appearance"]["window"]["background_image"] = serde_json::json!(invalid);
        assert!(!validator.is_valid(&settings), "{invalid}");
        assert!(super::parse_settings(&serde_json::to_vec(&settings).unwrap()).is_err());
    }
}
