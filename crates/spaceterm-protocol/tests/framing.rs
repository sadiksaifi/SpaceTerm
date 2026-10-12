use spaceterm_protocol::{
    ProtocolError,
    frame::{FrameDecoder, encode_frame},
    limits::MAX_FRAME_BYTES,
};

#[test]
fn fragmented_frames_preserve_payload_and_next_frame() {
    let first = encode_frame(b"first").unwrap();
    let second = encode_frame(b"second").unwrap();
    let mut decoder = FrameDecoder::default();
    for byte in &first[..first.len() - 1] {
        assert!(decoder.decode(&mut &[*byte][..]).unwrap().is_none());
    }
    let mut tail = [&first[first.len() - 1..], &second].concat();
    let mut input = tail.as_slice();
    assert_eq!(decoder.decode(&mut input).unwrap(), Some(b"first".to_vec()));
    assert_eq!(
        decoder.decode(&mut input).unwrap(),
        Some(b"second".to_vec())
    );
    assert!(input.is_empty());
    tail.clear();
}

#[test]
fn oversized_prefix_is_rejected_before_payload_buffering() {
    let mut decoder = FrameDecoder::default();
    let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
    assert_eq!(
        decoder.decode(&mut prefix.as_slice()),
        Err(ProtocolError::LimitExceeded)
    );
    assert_eq!(decoder.buffered_bytes(), 0);
    assert!(decoder.decode(&mut b"ignored".as_slice()).is_err());
}

#[test]
fn maximum_frame_is_accepted_and_plus_one_is_rejected() {
    let bytes = vec![1; MAX_FRAME_BYTES];
    let encoded = encode_frame(&bytes).unwrap();
    assert_eq!(
        FrameDecoder::default()
            .decode(&mut encoded.as_slice())
            .unwrap(),
        Some(bytes)
    );
    assert_eq!(
        encode_frame(&vec![1; MAX_FRAME_BYTES + 1]),
        Err(ProtocolError::LimitExceeded)
    );
}

#[test]
fn reduced_limits_and_zero_prefix_poison_the_decoder_without_consuming_later_bytes() {
    let mut decoder = FrameDecoder::with_limit(5).unwrap();
    assert_eq!(
        decoder.push(&encode_frame(b"12345").unwrap()).unwrap().1,
        Some(b"12345".to_vec())
    );
    assert_eq!(
        decoder.push(&6_u32.to_be_bytes()),
        Err(ProtocolError::LimitExceeded)
    );
    let mut input = b"unchanged".as_slice();
    assert_eq!(
        decoder.decode(&mut input),
        Err(ProtocolError::LimitExceeded)
    );
    assert_eq!(input, b"unchanged");
    assert_eq!(decoder.buffered_bytes(), 0);
    assert_eq!(
        FrameDecoder::new().push(&[0; 4]),
        Err(ProtocolError::Malformed)
    );
    assert_eq!(encode_frame(b""), Err(ProtocolError::Malformed));
}
