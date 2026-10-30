//! Transient Invitation Link output. Only a capable stdout receives the QR rendering.
use qrcodegen::{QrCode, QrCodeEcc};
use std::io::{self, IsTerminal, Write};

const QUIET_ZONE: i32 = 4;

pub(super) fn terminal_columns() -> Option<u16> {
    let output = io::stdout();
    if !output.is_terminal() {
        return None;
    }
    let term = std::env::var("TERM").ok()?;
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())?;
    if term.is_empty()
        || term == "dumb"
        || !locale
            .to_ascii_lowercase()
            .replace('-', "")
            .contains("utf8")
    {
        return None;
    }
    rustix::termios::tcgetwinsize(&output)
        .ok()
        .map(|size| size.ws_col)
}

pub(super) fn write_invitation(
    output: &mut impl Write,
    link: &str,
    columns: Option<u16>,
) -> io::Result<()> {
    writeln!(output, "{link}")?;
    if let Some(columns) = columns
        && let Ok(code) = QrCode::encode_text(link, QrCodeEcc::Medium)
        // Leave a column free to avoid right-margin wrapping.
        && code.size() + QUIET_ZONE * 2 < i32::from(columns)
    {
        let side = code.size() + QUIET_ZONE * 2;
        for y in (0..side).step_by(2) {
            // Fixed RGB colors keep dark modules on white in light and dark themes.
            write!(output, "\x1b[0;38;2;0;0;0;48;2;255;255;255m")?;
            for x in 0..side {
                let top = code.get_module(x - QUIET_ZONE, y - QUIET_ZONE);
                let bottom = code.get_module(x - QUIET_ZONE, y + 1 - QUIET_ZONE);
                let cell = match (top, bottom) {
                    (false, false) => ' ',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (true, true) => '█',
                };
                write!(output, "{cell}")?;
            }
            writeln!(output, "\x1b[0m")?;
        }
    }
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invitation() -> String {
        // Match the size and unbroken base64url of an invitation without issuing one.
        format!(
            "spaceterm://pair/{}",
            "eyJzY2hlbWEiOjEsImlkIjpbMTIsMzQsNTYsNzgsOTAsMTIs".repeat(20)
        )
    }

    fn render(link: &str, columns: Option<u16>) -> String {
        let mut output = Vec::new();
        write_invitation(&mut output, link, columns).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn nonterminal_unknown_width_and_narrow_output_get_only_the_link() {
        let link = invitation();
        for columns in [None, Some(0), Some(80)] {
            assert_eq!(render(&link, columns), format!("{link}\n"));
        }
    }

    #[test]
    fn an_invitation_too_large_for_a_qr_still_prints_its_link() {
        let link = format!("spaceterm://pair/{}", "x".repeat(8192));
        assert_eq!(render(&link, Some(240)), format!("{link}\n"));
    }

    #[test]
    fn a_capable_terminal_shows_a_scannable_qr_with_the_exact_link_payload() {
        let link = invitation();
        let output = render(&link, Some(240));
        let mut lines = output.lines();
        assert_eq!(lines.next(), Some(link.as_str()));
        let rows: Vec<Vec<char>> = lines
            .map(|line| {
                // Explicit RGB colors prevent either terminal theme from inverting the QR.
                line.strip_prefix("\x1b[0;38;2;0;0;0;48;2;255;255;255m")
                    .unwrap()
                    .strip_suffix("\x1b[0m")
                    .unwrap()
                    .chars()
                    .collect()
            })
            .collect();
        assert!(!rows.is_empty());
        let width = rows[0].len();
        assert!(width < 240);
        assert_eq!(rows.len(), width.div_ceil(2));
        assert!(rows.iter().all(|row| row.len() == width));

        // Rasterize the actual half-block output, including its quiet zone, for an
        // independent decoder. One terminal cell is one module wide and two high.
        const SCALE: usize = 4;
        let height = rows.len() * 2;
        let mut pixels = vec![255; width * height];
        for (y, row) in rows.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let (top, bottom) = match cell {
                    ' ' => (255, 255),
                    '▀' => (0, 255),
                    '▄' => (255, 0),
                    '█' => (0, 0),
                    _ => panic!("unexpected QR cell"),
                };
                pixels[y * 2 * width + x] = top;
                pixels[(y * 2 + 1) * width + x] = bottom;
            }
        }
        for y in 0..height {
            for x in 0..width {
                if x < 4 || x >= width - 4 || y < 4 || y >= height - 5 {
                    assert_eq!(pixels[y * width + x], 255, "four-module quiet zone");
                }
            }
        }
        let raster: Vec<u8> = (0..height * SCALE)
            .flat_map(|y| {
                let pixels = &pixels;
                (0..width * SCALE).map(move |x| pixels[y / SCALE * width + x / SCALE])
            })
            .collect();
        let mut decoder = quircs::Quirc::default();
        let codes: Vec<_> = decoder
            .identify(width * SCALE, height * SCALE, &raster)
            .collect();
        assert_eq!(codes.len(), 1);
        let decoded = codes[0].as_ref().unwrap().decode().unwrap();
        assert_eq!(decoded.payload, link.as_bytes());

        // Leave a spare column so the last QR cell cannot wrap at the right margin.
        assert_eq!(render(&link, Some(width as u16)), format!("{link}\n"));
        assert_eq!(render(&link, Some(width as u16 + 1)), output);
    }
}
