//! Transient Invitation Link output. Only a capable stdout receives the QR rendering.
use qrcodegen::{QrCode, QrCodeEcc};
use std::io::{self, IsTerminal, Write};

const QUIET_ZONE: i32 = 4;

#[derive(Clone, Copy)]
pub(super) struct Size {
    pub columns: u16,
    pub rows: u16,
}

pub(super) fn terminal_size() -> Option<Size> {
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
        .map(|size| Size {
            columns: size.ws_col,
            rows: size.ws_row,
        })
}

pub(super) fn write_invitation(
    output: &mut impl Write,
    link: &str,
    size: Option<Size>,
) -> io::Result<()> {
    writeln!(output, "{link}")?;
    if let Some(Size { columns, rows }) = size
        // Low correction chooses the smallest version; qrcodegen can strengthen it
        // without increasing that version.
        && let Ok(code) = QrCode::encode_text(link, QrCodeEcc::Low)
        // Leave a column free to avoid right-margin wrapping.
        && code.size() + QUIET_ZONE * 2 < i32::from(columns)
        && ((code.size() + QUIET_ZONE * 2) as usize).div_ceil(2)
            + super::prompt_rows(columns) <= usize::from(rows)
    {
        let side = code.size() + QUIET_ZONE * 2;
        for y in (0..side).step_by(2) {
            // Basic indexed colors also work in terminals without RGB support.
            write!(output, "\x1b[0;30;47m")?;
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
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        // A valid-format invitation with synthetic authority; no server issued it.
        let body = serde_json::json!({
            "schema": 1,
            "id": vec![255; 32],
            "endpoint": "quic://build.example.test:9231",
            "pin": {
                "build": "spaceterm-development",
                "server": vec![255; 32],
                "public_key": vec![255; 32],
                "certificate": vec![255; 32]
            },
            "server_name": "Build server",
            "token": vec![255; 32],
            "expires": 1730000000,
            "valid_for_seconds": 300
        });
        let link = format!(
            "spaceterm://pair/{}",
            URL_SAFE_NO_PAD.encode(body.to_string())
        );
        spaceterm_server::direct::Invitation::parse(&link)
            .expect("valid synthetic invitation")
            .display_text()
            .to_string()
    }

    fn render(link: &str, size: Option<Size>) -> String {
        let mut output = Vec::new();
        write_invitation(&mut output, link, size).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn nonterminal_unknown_width_and_narrow_output_get_only_the_link() {
        let link = invitation();
        for columns in [
            None,
            Some(Size {
                columns: 0,
                rows: 100,
            }),
            Some(Size {
                columns: 80,
                rows: 100,
            }),
        ] {
            assert_eq!(render(&link, columns), format!("{link}\n"));
        }
    }

    #[test]
    fn short_and_unknown_height_terminals_get_only_the_link() {
        let link = invitation();
        for rows in [0, 24] {
            assert_eq!(
                render(&link, Some(Size { columns: 140, rows })),
                format!("{link}\n")
            );
        }
    }

    #[test]
    fn the_smallest_qr_version_fits_when_medium_correction_would_not() {
        let link = format!("spaceterm://pair/{}", "x".repeat(960));
        let output = render(
            &link,
            Some(Size {
                columns: 114,
                rows: 80,
            }),
        );
        assert!(output.contains('█'));
    }

    #[test]
    fn the_qr_leaves_room_for_waiting_and_comparison_code_entry() {
        let link = invitation();
        let output = render(
            &link,
            Some(Size {
                columns: 240,
                rows: 100,
            }),
        );
        let qr_rows = output.lines().count() as u16 - 1;
        assert_eq!(
            render(
                &link,
                Some(Size {
                    columns: 240,
                    rows: qr_rows + 2
                })
            ),
            format!("{link}\n")
        );
        assert_eq!(
            render(
                &link,
                Some(Size {
                    columns: 240,
                    rows: qr_rows + 3
                })
            ),
            output
        );
    }

    #[test]
    fn a_narrow_terminal_reserves_wrapped_prompts() {
        let link = "spaceterm://pair/test-invitation";
        assert_eq!(
            render(
                link,
                Some(Size {
                    columns: 40,
                    rows: 25
                })
            ),
            format!("{link}\n")
        );
        assert!(
            render(
                link,
                Some(Size {
                    columns: 40,
                    rows: 26
                })
            )
            .contains('█')
        );
    }

    #[test]
    fn an_invitation_too_large_for_a_qr_still_prints_its_link() {
        let link = format!("spaceterm://pair/{}", "x".repeat(8192));
        assert_eq!(
            render(
                &link,
                Some(Size {
                    columns: 240,
                    rows: 100
                })
            ),
            format!("{link}\n")
        );
    }

    #[test]
    fn a_capable_terminal_shows_a_scannable_qr_with_the_exact_link_payload() {
        let link = invitation();
        let output = render(
            &link,
            Some(Size {
                columns: 240,
                rows: 100,
            }),
        );
        let mut lines = output.lines();
        assert_eq!(lines.next(), Some(link.as_str()));
        let rows: Vec<Vec<char>> = lines
            .map(|line| {
                // Basic ANSI colors work in GNU Screen without true-color support.
                line.strip_prefix("\x1b[0;30;47m")
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
        assert_eq!(
            render(
                &link,
                Some(Size {
                    columns: width as u16,
                    rows: 100
                })
            ),
            format!("{link}\n")
        );
        assert_eq!(
            render(
                &link,
                Some(Size {
                    columns: width as u16 + 1,
                    rows: 100
                })
            ),
            output
        );
    }
}
