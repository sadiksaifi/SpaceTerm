//! Real-host smoke check, using the same native credentials and authenticated transport as desktop.
use spaceterm::runtime::PhysicalKey;
use spaceterm_server::{ServerError, pane::*};
use std::{
    error::Error,
    time::{Duration, Instant},
};

pub fn command(
    connection: &mut PaneConnection,
    lease: Lease,
    text: &str,
) -> Result<(), ServerError> {
    connection.exchange(&Request::Key {
        lease,
        key: Key {
            physical: PhysicalKey::Unidentified as u32,
            action: 0,
            logical: "text-input".into(),
            text: Some(text.into()),
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        },
    })?;
    connection.exchange(&Request::Key {
        lease,
        key: Key {
            physical: PhysicalKey::Enter as u32,
            action: 0,
            logical: "enter".into(),
            text: None,
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        },
    })?;
    Ok(())
}
pub fn attach(connection: &mut PaneConnection, create: bool) -> Result<Lease, ServerError> {
    match connection.exchange(&Request::Attach {
        create,
        resume: None,
        style: None,
        size: PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 18,
        },
    })? {
        Reply::Attached { lease } => Ok(lease),
        _ => Err(ServerError::Protocol),
    }
}
pub fn await_output(
    connection: &mut PaneConnection,
    lease: Lease,
    marker: &str,
) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Reply::Screen(screen) = connection.exchange(&Request::Poll { lease })? {
            let output = screen
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if output.lines().any(|line| line.trim_end() == marker) {
                return Ok(output);
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("terminal output did not arrive".into())
}
