use super::*;

/// A paired server that reported a stopped update on its own computer. Its row only opens that
/// server's Settings.
fn paired_attention() -> ServerUpdateRow {
    server(
        "penguin",
        ServerUpdateAction::Review(MachineTarget::Direct([3; 32])),
    )
}

#[test]
fn genuine_offers_raise_server_pills_without_changing_their_targets() {
    assert_eq!(
        pill(None, None, &[server("machine", ServerUpdateAction::Local)]),
        Some((
            "Update Server".into(),
            PillTone::Primary,
            None,
            PillCommand::Server(ServerUpdateAction::Local)
        ))
    );
    assert_eq!(
        pill(
            None,
            None,
            &[
                server("local", ServerUpdateAction::Local),
                paired_attention()
            ]
        ),
        Some((
            "Review 2 Servers".into(),
            PillTone::Primary,
            None,
            PillCommand::Menu
        ))
    );
}

#[test]
fn only_a_genuine_server_offer_raises_client_progress_and_retry() {
    let offer = server("local", ServerUpdateAction::Local);
    let attention = paired_attention();
    for state in [
        UpdateState::Downloading {
            version: version(),
            received: 1,
            total: 4,
        },
        UpdateState::Verifying { version: version() },
        UpdateState::Failed {
            error: UpdateError::Download,
        },
    ] {
        let baseline = pill(Some(state.clone()), None, &[]).unwrap();
        for (rows, tone) in [
            (quiet_servers(), PillTone::Neutral),
            (vec![attention.clone()], PillTone::Neutral),
            (vec![offer.clone()], PillTone::Primary),
            (vec![attention.clone(), offer.clone()], PillTone::Primary),
        ] {
            let actual = pill(Some(state.clone()), None, &rows).unwrap();
            assert_eq!(
                actual,
                (baseline.0.clone(), tone, baseline.2, baseline.3.clone()),
                "{state:?}"
            );
            assert_eq!(
                UpdatePill::resolve(resolve(state.clone()), &rows)
                    .unwrap()
                    .marked(),
                rows.iter().any(|row| row
                    .action
                    .as_ref()
                    .and_then(ServerUpdateAction::need)
                    .is_some())
            );
        }
    }
}

#[test]
fn server_attention_counts_preserve_existing_warning_precedence() {
    let attention = server("local", ServerUpdateAction::Review(MachineTarget::Local));
    let other_attention = paired_attention();
    let offer = server("local", ServerUpdateAction::Local);
    assert_eq!(
        pill(None, None, std::slice::from_ref(&attention))
            .unwrap()
            .1,
        PillTone::Warning
    );
    assert_eq!(
        pill(None, None, &[attention.clone(), other_attention.clone()]).unwrap(),
        (
            "Review 2 Servers".into(),
            PillTone::Neutral,
            None,
            PillCommand::Menu
        )
    );
    assert_eq!(
        pill(None, None, &[other_attention.clone(), offer.clone()]).unwrap(),
        (
            "Review 2 Servers".into(),
            PillTone::Primary,
            None,
            PillCommand::Menu
        )
    );
    assert_eq!(
        pill(
            Some(UpdateState::Ready { version: version() }),
            Some(UpdateStage::Overdue),
            &[other_attention, offer]
        ),
        Some((
            "Install 0.4.2".into(),
            PillTone::Warning,
            Some(ControlGlyph::Warning),
            PillCommand::Client(Some(ControlCommand::RequestInstall))
        ))
    );
}
