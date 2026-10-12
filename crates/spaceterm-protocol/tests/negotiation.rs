use spaceterm_protocol::{
    ProtocolError,
    negotiation::{Negotiator, SUPPORTED_MAJORS},
};

#[test]
fn negotiation_selects_current_and_complete_previous_major() {
    assert_eq!(
        Negotiator::select(&SUPPORTED_MAJORS, &SUPPORTED_MAJORS),
        Ok(2)
    );
    assert_eq!(Negotiator::select(&[1], &SUPPORTED_MAJORS), Ok(1));
    assert_eq!(Negotiator::select(&SUPPORTED_MAJORS, &[1]), Ok(1));
    assert_eq!(
        Negotiator::select(&[3], &SUPPORTED_MAJORS),
        Err(ProtocolError::Incompatible)
    );
    assert_eq!(
        Negotiator::verify(&SUPPORTED_MAJORS, &SUPPORTED_MAJORS, 1),
        Err(ProtocolError::Negotiation)
    );
}

#[test]
fn invalid_advertisements_are_not_repaired_and_newer_unknown_peer_profiles_are_ignored() {
    for advertisement in [
        vec![],
        vec![0],
        vec![1, 2],
        vec![2, 2],
        (1..=17).rev().collect(),
    ] {
        assert_eq!(
            Negotiator::select(&advertisement, &SUPPORTED_MAJORS),
            Err(ProtocolError::Malformed)
        );
    }
    assert_eq!(Negotiator::select(&SUPPORTED_MAJORS, &[3, 2, 1]), Ok(2));
}
