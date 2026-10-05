//! Draws an identity's application icon from its Icon Composer document.
//!
//! Hosts with a compiled bundle icon show that icon. Elsewhere SpaceTerm composes the same document
//! as one full-color SVG: the document's fill on the icon body, each layer's artwork in its
//! light-appearance fill, and the layer shadow and translucency the document asks for. The glass
//! material itself is approximated as a flat, partly translucent fill under a soft rim highlight.

use std::fmt::Write as _;

use serde::Deserialize;

use crate::application_identity::ApplicationIcon;

/// The Icon Composer grid: a 1024-point canvas whose body is an 824-point continuous-corner square
/// centered on it, leaving the margin the system icon shadow falls into.
const CANVAS: f32 = 1024.0;
const BODY_ORIGIN: f32 = 100.0;
const BODY_SIZE: f32 = 824.0;
const BODY_CORNER_RADIUS: f32 = 185.4;

/// Why a document could not be drawn. The variants carry no document content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ApplicationIconError {
    #[error("the icon document is malformed")]
    MalformedDocument,
    #[error("the icon document uses an unsupported color")]
    UnsupportedColor,
    #[error("the icon document names artwork that is not embedded")]
    MissingArtwork,
    #[error("the icon artwork is malformed")]
    MalformedArtwork,
}

#[derive(Deserialize)]
struct Document {
    fill: Fill,
    groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Fill {
    #[serde(rename = "linear-gradient")]
    linear_gradient: Option<[String; 2]>,
    solid: Option<String>,
}

#[derive(Deserialize)]
struct Group {
    layers: Vec<Layer>,
    shadow: Option<Shadow>,
    translucency: Option<Translucency>,
}

#[derive(Deserialize)]
struct Layer {
    #[serde(rename = "image-name")]
    image_name: String,
    #[serde(default)]
    hidden: bool,
    #[serde(rename = "fill-specializations", default)]
    fill_specializations: Vec<FillSpecialization>,
}

#[derive(Deserialize)]
struct FillSpecialization {
    appearance: Option<String>,
    value: serde_json::Value,
}

/// A group's shadow onto the layers below it. SpaceTerm draws every kind as the neutral shadow.
#[derive(Deserialize)]
struct Shadow {
    opacity: f32,
}

#[derive(Deserialize)]
struct Translucency {
    enabled: bool,
    value: f32,
}

/// An sRGB color with straight alpha, each component in `0..=1`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Srgb {
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
}

impl Srgb {
    const WHITE: Self = Self {
        red: 1.0,
        green: 1.0,
        blue: 1.0,
        alpha: 1.0,
    };
    const BLACK: Self = Self {
        red: 0.0,
        green: 0.0,
        blue: 0.0,
        alpha: 1.0,
    };

    /// Parses an Icon Composer color such as `display-p3:0.31689,0.38330,0.81055,1.00000`.
    fn parse(value: &str) -> Result<Self, ApplicationIconError> {
        let (space, components) = value
            .split_once(':')
            .ok_or(ApplicationIconError::UnsupportedColor)?;
        let components = components
            .split(',')
            .map(|component| component.trim().parse::<f32>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ApplicationIconError::UnsupportedColor)?;
        let [red, green, blue, alpha] = components[..] else {
            return Err(ApplicationIconError::UnsupportedColor);
        };
        match space {
            "srgb" | "extended-srgb" => Ok(Self {
                red,
                green,
                blue,
                alpha,
            }
            .clamped()),
            "display-p3" => Ok(Self::from_display_p3(red, green, blue, alpha)),
            _ => Err(ApplicationIconError::UnsupportedColor),
        }
    }

    /// Display P3 and sRGB share their primaries' white point and transfer function, so the
    /// conversion is the linear-light primaries matrix between the two encodings.
    fn from_display_p3(red: f32, green: f32, blue: f32, alpha: f32) -> Self {
        let [red, green, blue] = [red, green, blue].map(decode);
        let linear = [
            1.224_940_2 * red - 0.224_940_4 * green,
            -0.042_056_955 * red + 1.042_057 * green,
            -0.019_637_555 * red - 0.078_636_05 * green + 1.098_273_6 * blue,
        ];
        let [red, green, blue] = linear.map(encode);
        Self {
            red,
            green,
            blue,
            alpha,
        }
        .clamped()
    }

    fn clamped(self) -> Self {
        Self {
            red: self.red.clamp(0.0, 1.0),
            green: self.green.clamp(0.0, 1.0),
            blue: self.blue.clamp(0.0, 1.0),
            alpha: self.alpha.clamp(0.0, 1.0),
        }
    }

    fn hex(self) -> String {
        let channel = |value: f32| (value * 255.0).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}",
            channel(self.red),
            channel(self.green),
            channel(self.blue)
        )
    }
}

fn decode(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(linear: f32) -> f32 {
    let linear = linear.clamp(0.0, 1.0);
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// The light-appearance fill of one layer: its unspecialized fill, which the system light fill
/// and automatic fills draw as white.
fn layer_fill(layer: &Layer) -> Result<Srgb, ApplicationIconError> {
    let Some(specialization) = layer
        .fill_specializations
        .iter()
        .find(|specialization| specialization.appearance.is_none())
    else {
        return Ok(Srgb::WHITE);
    };
    match &specialization.value {
        serde_json::Value::String(value) if value == "system-dark" => Ok(Srgb::BLACK),
        serde_json::Value::String(_) => Ok(Srgb::WHITE),
        serde_json::Value::Object(value) => value
            .get("solid")
            .and_then(serde_json::Value::as_str)
            .ok_or(ApplicationIconError::UnsupportedColor)
            .and_then(Srgb::parse),
        _ => Err(ApplicationIconError::UnsupportedColor),
    }
}

/// The artwork's view box and the markup inside its root element.
fn artwork_parts(artwork: &str) -> Result<(&str, &str), ApplicationIconError> {
    let root = artwork
        .find("<svg")
        .ok_or(ApplicationIconError::MalformedArtwork)?;
    let open_end = root
        + artwork[root..]
            .find('>')
            .ok_or(ApplicationIconError::MalformedArtwork)?;
    let close = artwork
        .rfind("</svg>")
        .filter(|close| *close > open_end)
        .ok_or(ApplicationIconError::MalformedArtwork)?;
    let open = &artwork[root..open_end];
    let view_box = open
        .split_once("viewBox=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(view_box, _)| view_box)
        .ok_or(ApplicationIconError::MalformedArtwork)?;
    Ok((view_box, &artwork[open_end + 1..close]))
}

/// Apple's continuous-corner rounded square, the shape of every Icon Composer icon body.
///
/// Each corner blends into its edges over 1.528665 radii through three cubic segments, which keeps
/// curvature continuous where a circular corner would jump.
fn continuous_square(origin: f32, size: f32, radius: f32) -> String {
    let (left, top, right, bottom) = (origin, origin, origin + size, origin + size);
    let r = radius;
    let mut path = String::new();
    let mut point = |command: &str, coordinates: &[(f32, f32)]| {
        path.push_str(command);
        for (x, y) in coordinates {
            let _ = write!(path, "{x:.3} {y:.3} ");
        }
    };
    point("M", &[(left + 1.528_665 * r, top)]);
    point("L", &[(right - 1.528_665 * r, top)]);
    point(
        "C",
        &[
            (right - 1.088_493 * r, top),
            (right - 0.868_407 * r, top + 0.020_443 * r),
            (right - 0.631_494 * r, top + 0.074_911 * r),
        ],
    );
    point(
        "C",
        &[
            (right - 0.372_824 * r, top + 0.162_016 * r),
            (right - 0.162_016 * r, top + 0.372_824 * r),
            (right - 0.074_911 * r, top + 0.631_494 * r),
        ],
    );
    point(
        "C",
        &[
            (right - 0.020_443 * r, top + 0.868_407 * r),
            (right, top + 1.088_493 * r),
            (right, top + 1.528_665 * r),
        ],
    );
    point("L", &[(right, bottom - 1.528_665 * r)]);
    point(
        "C",
        &[
            (right, bottom - 1.088_493 * r),
            (right - 0.020_443 * r, bottom - 0.868_407 * r),
            (right - 0.074_911 * r, bottom - 0.631_494 * r),
        ],
    );
    point(
        "C",
        &[
            (right - 0.162_016 * r, bottom - 0.372_824 * r),
            (right - 0.372_824 * r, bottom - 0.162_016 * r),
            (right - 0.631_494 * r, bottom - 0.074_911 * r),
        ],
    );
    point(
        "C",
        &[
            (right - 0.868_407 * r, bottom - 0.020_443 * r),
            (right - 1.088_493 * r, bottom),
            (right - 1.528_665 * r, bottom),
        ],
    );
    point("L", &[(left + 1.528_665 * r, bottom)]);
    point(
        "C",
        &[
            (left + 1.088_493 * r, bottom),
            (left + 0.868_407 * r, bottom - 0.020_443 * r),
            (left + 0.631_494 * r, bottom - 0.074_911 * r),
        ],
    );
    point(
        "C",
        &[
            (left + 0.372_824 * r, bottom - 0.162_016 * r),
            (left + 0.162_016 * r, bottom - 0.372_824 * r),
            (left + 0.074_911 * r, bottom - 0.631_494 * r),
        ],
    );
    point(
        "C",
        &[
            (left + 0.020_443 * r, bottom - 0.868_407 * r),
            (left, bottom - 1.088_493 * r),
            (left, bottom - 1.528_665 * r),
        ],
    );
    point("L", &[(left, top + 1.528_665 * r)]);
    point(
        "C",
        &[
            (left, top + 1.088_493 * r),
            (left + 0.020_443 * r, top + 0.868_407 * r),
            (left + 0.074_911 * r, top + 0.631_494 * r),
        ],
    );
    point(
        "C",
        &[
            (left + 0.162_016 * r, top + 0.372_824 * r),
            (left + 0.372_824 * r, top + 0.162_016 * r),
            (left + 0.631_494 * r, top + 0.074_911 * r),
        ],
    );
    point(
        "C",
        &[
            (left + 0.868_407 * r, top + 0.020_443 * r),
            (left + 1.088_493 * r, top),
            (left + 1.528_665 * r, top),
        ],
    );
    path.push('Z');
    path
}

/// Composes `icon` as a standalone SVG drawn at `size` logical pixels square.
pub(crate) fn svg(icon: ApplicationIcon, size: f32) -> Result<String, ApplicationIconError> {
    let document: Document =
        serde_json::from_str(icon.document).map_err(|_| ApplicationIconError::MalformedDocument)?;
    let [top, bottom] = match (&document.fill.linear_gradient, &document.fill.solid) {
        (Some([top, bottom]), _) => [Srgb::parse(top)?, Srgb::parse(bottom)?],
        (None, Some(solid)) => [Srgb::parse(solid)?; 2],
        (None, None) => [Srgb::WHITE; 2],
    };
    let body = continuous_square(BODY_ORIGIN, BODY_SIZE, BODY_CORNER_RADIUS);
    let mut out = String::new();
    let _ = write!(
        out,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {CANVAS} {CANVAS}"><defs><clipPath id="body"><path d="{body}"/></clipPath><linearGradient id="fill" x1="0" y1="{BODY_ORIGIN}" x2="0" y2="{body_bottom}" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="{top}" stop-opacity="{top_alpha}"/><stop offset="1" stop-color="{bottom}" stop-opacity="{bottom_alpha}"/></linearGradient><linearGradient id="rim" x1="0" y1="{BODY_ORIGIN}" x2="0" y2="{body_bottom}" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="#ffffff" stop-opacity="0.55"/><stop offset="0.5" stop-color="#ffffff" stop-opacity="0.12"/><stop offset="1" stop-color="#ffffff" stop-opacity="0.3"/></linearGradient><filter id="icon-shadow" x="0" y="0" width="{CANVAS}" height="{CANVAS}" filterUnits="userSpaceOnUse"><feDropShadow dx="0" dy="12" stdDeviation="12" flood-color="#000000" flood-opacity="0.3"/></filter>"##,
        body_bottom = BODY_ORIGIN + BODY_SIZE,
        top = top.hex(),
        top_alpha = top.alpha,
        bottom = bottom.hex(),
        bottom_alpha = bottom.alpha,
    );
    let mut layers = String::new();
    let mut mask = 0;
    for group in &document.groups {
        let translucency = group
            .translucency
            .as_ref()
            .filter(|translucency| translucency.enabled)
            .map_or(0.0, |translucency| translucency.value.clamp(0.0, 1.0));
        // Drawn flat, glass shows half of its translucency: the material frosts what it lets through.
        let layer_opacity = 1.0 - translucency / 2.0;
        let filter = group.shadow.as_ref().map(|shadow| {
            let id = format!("layer-shadow-{mask}");
            let _ = write!(
                out,
                r##"<filter id="{id}" x="0" y="0" width="{CANVAS}" height="{CANVAS}" filterUnits="userSpaceOnUse"><feDropShadow dx="0" dy="10" stdDeviation="14" flood-color="#000000" flood-opacity="{opacity}"/></filter>"##,
                opacity = (shadow.opacity * 0.5).clamp(0.0, 1.0),
            );
            id
        });
        let mut group_layers = String::new();
        // A document lists its topmost layer first.
        for layer in group.layers.iter().rev().filter(|layer| !layer.hidden) {
            let artwork = icon
                .artwork
                .iter()
                .find_map(|(name, artwork)| (*name == layer.image_name).then_some(*artwork))
                .ok_or(ApplicationIconError::MissingArtwork)?;
            let (view_box, content) = artwork_parts(artwork)?;
            let fill = layer_fill(layer)?;
            let id = format!("layer-{mask}");
            mask += 1;
            let _ = write!(
                out,
                r#"<mask id="{id}" mask-type="alpha" maskUnits="userSpaceOnUse" x="0" y="0" width="{CANVAS}" height="{CANVAS}"><svg x="{BODY_ORIGIN}" y="{BODY_ORIGIN}" width="{BODY_SIZE}" height="{BODY_SIZE}" viewBox="{view_box}">{content}</svg></mask>"#,
            );
            let _ = write!(
                group_layers,
                r#"<rect x="{BODY_ORIGIN}" y="{BODY_ORIGIN}" width="{BODY_SIZE}" height="{BODY_SIZE}" fill="{color}" fill-opacity="{opacity}" mask="url(#{id})"/>"#,
                color = fill.hex(),
                opacity = fill.alpha * layer_opacity,
            );
        }
        match filter {
            Some(filter) => {
                let _ = write!(layers, r#"<g filter="url(#{filter})">{group_layers}</g>"#);
            }
            None => layers.push_str(&group_layers),
        }
    }
    let _ = write!(
        out,
        r#"</defs><path d="{body}" fill="url(#fill)" filter="url(#icon-shadow)"/><g clip-path="url(#body)">{layers}<path d="{body}" fill="none" stroke="url(#rim)" stroke-width="12"/></g></svg>"#,
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::application_identity::ApplicationIdentity;

    /// Renders `icon` as GPUI does and returns a pixel sampler over the 1024-point icon canvas.
    fn rendered(icon: ApplicationIcon) -> impl Fn(f32, f32) -> [u8; 4] {
        let svg = svg(icon, 64.0).expect("the icon should compose");
        let image = gpui::Image::from_bytes(gpui::ImageFormat::Svg, svg.into_bytes())
            .to_image_data(gpui::SvgRenderer::new(Arc::new(())))
            .expect("the composed icon should render");
        let size = image.size(0);
        let (width, height) = (size.width.0 as usize, size.height.0 as usize);
        assert_eq!(
            (width, height),
            (128, 128),
            "drawn at twice its logical size"
        );
        let bytes = image.as_bytes(0).unwrap().to_vec();
        move |x, y| {
            let column = (x / CANVAS * width as f32) as usize;
            let row = (y / CANVAS * height as f32) as usize;
            let index = (row * width + column) * 4;
            // GPUI stores rendered images as BGRA.
            [
                bytes[index + 2],
                bytes[index + 1],
                bytes[index],
                bytes[index + 3],
            ]
        }
    }

    /// Maps a point in the 1025-point layer artwork onto the icon canvas.
    fn artwork(x: f32, y: f32) -> (f32, f32) {
        let scale = BODY_SIZE / 1025.0;
        (BODY_ORIGIN + x * scale, BODY_ORIGIN + y * scale)
    }

    #[test]
    fn display_p3_colors_convert_to_srgb() {
        let white = Srgb::parse("display-p3:1,1,1,1").unwrap();
        assert!(
            [white.red, white.green, white.blue]
                .iter()
                .all(|c| (c - 1.0).abs() < 1e-3)
        );
        // Pure P3 red lies outside sRGB and clamps to its red primary.
        let red = Srgb::parse("display-p3:1,0,0,1").unwrap();
        assert_eq!(red.hex(), "#ff0000");
        let release = Srgb::parse("display-p3:0.31689,0.38330,0.81055,1.00000").unwrap();
        assert_eq!(release.hex(), "#4c62d6");
        assert_eq!(
            Srgb::parse("hsl:1,2,3,4"),
            Err(ApplicationIconError::UnsupportedColor)
        );
        assert_eq!(
            Srgb::parse("display-p3:1,1,1"),
            Err(ApplicationIconError::UnsupportedColor)
        );
    }

    #[test]
    fn every_identity_icon_draws_its_body_glyph_and_margin() {
        for identity in crate::application_identity::testing::all() {
            let pixel = rendered(identity.icon());
            // The grid margin stays clear for the icon shadow.
            assert_eq!(pixel(4.0, 4.0)[3], 0);
            // The body's corner is continuous rather than square.
            assert_eq!(pixel(BODY_ORIGIN + 4.0, BODY_ORIGIN + 4.0)[3], 0);
            // The terminal area shows the document's fill.
            let (x, y) = artwork(512.0, 420.0);
            let fill = pixel(x, y);
            assert_eq!(fill[3], 255);
            assert!(
                fill[0] < 200 || fill[2] < 200,
                "fill shows through: {fill:?}"
            );
            // The window's title bar is the light glyph.
            let (x, y) = artwork(340.0, 259.0);
            let glyph = pixel(x, y);
            assert!(
                glyph.iter().all(|channel| *channel > 150),
                "glyph: {glyph:?}"
            );
        }
    }

    #[test]
    fn channel_marks_rank_the_preflight_and_development_icons() {
        let mark = artwork(838.5, 627.0);
        let upper = artwork(838.5, 514.0);
        let light = |pixel: [u8; 4]| pixel[..3].iter().all(|channel| *channel > 150);
        let identities = crate::application_identity::testing::all();
        let marks = identities.map(|identity| {
            let pixel = rendered(identity.icon());
            (light(pixel(mark.0, mark.1)), light(pixel(upper.0, upper.1)))
        });
        // Release: no mark. Preflight: one centered mark. Development: two marks.
        assert_eq!(marks, [(false, false), (true, false), (false, true)]);
    }

    #[test]
    fn a_document_naming_missing_artwork_is_rejected() {
        let icon = ApplicationIcon {
            document: ApplicationIdentity::current().icon().document,
            artwork: &[],
        };
        assert_eq!(svg(icon, 64.0), Err(ApplicationIconError::MissingArtwork));
        let icon = ApplicationIcon {
            document: "{",
            artwork: &[],
        };
        assert_eq!(
            svg(icon, 64.0),
            Err(ApplicationIconError::MalformedDocument)
        );
    }
}
