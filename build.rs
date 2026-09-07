use std::{env, fs, path::PathBuf};

#[expect(dead_code, reason = "the build uses only color parsing and conversion")]
#[path = "src/theme/color.rs"]
mod color;

fn main() -> Result<(), &'static str> {
    const SOURCE: &str = "third_party/vague-pro-zed/themes/vague-pro.json";
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed=src/theme/tokens.rs");
    println!("cargo:rerun-if-changed=src/theme/color.rs");
    let json = fs::read_to_string(SOURCE)
        .map_err(|_| "Default theme missing; run git submodule update --init --recursive")?;
    let family: serde_json::Value =
        serde_json::from_str(&json).map_err(|_| "Default theme is not valid JSON")?;
    let theme = family["themes"]
        .as_array()
        .and_then(|themes| themes.iter().find(|theme| theme["name"] == "Vague Pro"))
        .ok_or("Default theme family does not contain Vague Pro")?;
    if theme["appearance"] != "dark" {
        return Err("Default theme appearance changed");
    }
    let style = &theme["style"];
    let paint = |value: &serde_json::Value| -> Result<String, &'static str> {
        let color = value.as_str().ok_or("Default theme color is missing")?;
        let color = color::Color::parse(color).map_err(|_| "Invalid default theme color")?;
        Ok(format!("Color::rgba(0x{:08x})", color.rgba_hex()))
    };
    let mut output = String::from(
        "pub(crate) static VAGUE_PRO: LazyLock<Theme> = LazyLock::new(|| Theme {\nname: String::from(\"Vague Pro\"), appearance: Appearance::Dark,\n",
    );
    macro_rules! theme_colors {
        ($($field:ident => $key:literal,)*) => {{
            $(output.push_str(&format!("{}: {},\n", stringify!($field), paint(&style[$key])?));)*
        }};
    }
    include!("src/theme/tokens.rs");
    output.push_str(&format!(
        "modal_scrim: Color {{ a: 0x99, ..{} }},\n",
        paint(&style["background"])?
    ));
    output.push_str("accents: vec![");
    for value in style["accents"]
        .as_array()
        .ok_or("Default accents missing")?
    {
        output.push_str(&format!("{},", paint(value)?));
    }
    output.push_str("], players: vec![");
    let players = style["players"]
        .as_array()
        .filter(|players| !players.is_empty())
        .ok_or("Default player colors missing")?;
    for player in players {
        output.push_str(&format!(
            "PlayerTheme {{ background: {}, cursor: {}, selection: {} }},",
            paint(&player["background"])?,
            paint(&player["cursor"])?,
            paint(&player["selection"])?
        ));
    }
    output.push_str("], syntax: serde_json::Value::Object([");
    for (name, style) in style["syntax"]
        .as_object()
        .ok_or("Default syntax missing")?
    {
        output.push_str(&format!(
            "({name:?}.to_owned(), serde_json::json!({style})),"
        ));
    }
    output.push_str("].into_iter().collect()),\n});\n");
    let directory = env::var_os("OUT_DIR").ok_or("Build output directory missing")?;
    fs::write(PathBuf::from(directory).join("default_theme.rs"), output)
        .map_err(|_| "Cannot write generated default theme")
}
