//! Owned by server-core Job B. Implements the one qualified identity conversion.
//!
//! ServerId and DeviceId retain their exact 16 bytes. For legacy Principal and
//! OperationId local handles, PairingId, ClientId and OperationStreamKey encode
//! eight zero bytes followed by the nonzero local u64 in big-endian order. The
//! surrounding ServerId supplies qualification. Decoders reject every other
//! representation. New OperationStream uses Entity directly and no sequence.
//! All duplicate representations must agree; none confers authentication.
