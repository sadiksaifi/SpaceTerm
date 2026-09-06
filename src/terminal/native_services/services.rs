//! Synchronous Services policy and operation ownership, independent of native responders.

use std::cell::Cell;
use std::rc::Rc;

use crate::terminal::{
    MAX_PASTE_BYTES, NativeServiceCapabilities, NativeServiceOrigin, NativeServiceStatus,
    SelectionCopy,
};

/// Application composition binds this endpoint to one exact Operating-System Window.
/// Each callback revalidates the terminal origin before reading or inserting text.
pub(crate) trait ServiceEndpoint {
    fn status(&self) -> NativeServiceStatus;
    fn selection(&self, origin: NativeServiceOrigin) -> Option<SelectionCopy>;
    fn insert_text(&self, origin: NativeServiceOrigin, text: String) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServiceDataType {
    Absent,
    String,
    Unsupported,
}

impl ServiceDataType {
    const fn is_requested(self) -> bool {
        !matches!(self, Self::Absent)
    }

    const fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

/// Opaque native transfer identity, compared only and never dereferenced by portable policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServicePasteboardIdentity(usize);

impl ServicePasteboardIdentity {
    pub(crate) const fn new(value: usize) -> Self {
        Self(value)
    }
}

pub(crate) struct ServiceRequests {
    endpoint: Rc<dyn ServiceEndpoint>,
    next_request_id: Cell<u64>,
    owner_alive: Rc<Cell<bool>>,
    // Services callbacks are synchronous on the UI thread. Cell avoids retaining a mutable
    // borrow across endpoint or native callbacks, which may reenter the request machinery.
    active_request: Rc<Cell<Option<u64>>>,
}

impl ServiceRequests {
    pub(crate) fn new(endpoint: Rc<dyn ServiceEndpoint>) -> Self {
        Self {
            endpoint,
            next_request_id: Cell::new(0),
            owner_alive: Rc::new(Cell::new(true)),
            active_request: Rc::new(Cell::new(None)),
        }
    }

    pub(crate) fn operation(
        &self,
        send_type: ServiceDataType,
        return_type: ServiceDataType,
    ) -> Option<ServiceOperation> {
        if !self.owner_alive.get() {
            return None;
        }
        let status = self.endpoint.status();
        if !self.owner_alive.get()
            || !accepts_service_request(status.capabilities, send_type, return_type)
        {
            return None;
        }
        let validated_origin = status.origin?;
        let request_id = self.next_request_id.get().checked_add(1)?;
        self.next_request_id.set(request_id);
        Some(ServiceOperation {
            endpoint: Rc::clone(&self.endpoint),
            owner_alive: Rc::clone(&self.owner_alive),
            validated_origin,
            identity: ServiceOperationIdentity {
                request_id,
                returns_text: return_type.is_requested(),
                active_request: Rc::clone(&self.active_request),
                write_claimed: Cell::new(false),
                pasteboard: Cell::new(None),
                origin: Cell::new(None),
            },
        })
    }
}

impl ServiceRequests {
    pub(crate) fn is_retired(&self) -> bool {
        !self.owner_alive.get()
    }

    pub(crate) fn retire(&self) {
        // Native callbacks retain this owner locally to survive reentrant responder destruction.
        // Retirement revokes its operations immediately, before the last retained owner drops.
        self.owner_alive.set(false);
        self.active_request.set(None);
    }
}

impl Drop for ServiceRequests {
    fn drop(&mut self) {
        self.retire();
    }
}

pub(crate) struct ServiceOperation {
    endpoint: Rc<dyn ServiceEndpoint>,
    owner_alive: Rc<Cell<bool>>,
    validated_origin: NativeServiceOrigin,
    identity: ServiceOperationIdentity,
}

impl ServiceOperation {
    /// Publishes selection synchronously before the native request callback returns.
    pub(crate) fn write_selection(
        &self,
        pasteboard: ServicePasteboardIdentity,
        write: impl FnOnce(&str) -> bool,
    ) -> bool {
        if !self.owner_alive.get() || !self.identity.claim_write() {
            return false;
        }
        // A native or endpoint callback can unwind. Release its reservation even when the
        // operation responder remains retained by the host after that failure.
        let mut reservation = OperationReservation {
            identity: &self.identity,
            completed: false,
        };
        let status = self.endpoint.status();
        if !self.owner_alive.get()
            || status.origin != Some(self.validated_origin)
            || !status.capabilities.send_text
        {
            return false;
        }
        let Some(selection) = self.endpoint.selection(self.validated_origin) else {
            return false;
        };
        if !self.owner_alive.get() {
            return false;
        }
        let wrote = write(&selection.plain_text) && self.owner_alive.get();
        self.identity
            .finish_send(self.validated_origin, pasteboard, wrote);
        reservation.completed = true;
        wrote
    }

    pub(crate) fn read_selection(
        &self,
        pasteboard: ServicePasteboardIdentity,
        read: impl FnOnce() -> Option<String>,
    ) -> bool {
        if !self.owner_alive.get() {
            return false;
        }
        let Some(origin) = self.identity.take_return_origin(pasteboard) else {
            return false;
        };
        // Consume this operation's authority before callbacks, while retaining the shared
        // reservation until reading and insertion finish so reentrancy cannot replace its data.
        let _reservation = OperationReservation {
            identity: &self.identity,
            completed: false,
        };
        let status = self.endpoint.status();
        if !self.owner_alive.get()
            || origin != self.validated_origin
            || status.origin != Some(self.validated_origin)
            || !status.capabilities.return_text
        {
            return false;
        }
        let Some(text) = read() else {
            return false;
        };
        if !self.owner_alive.get() || text.len() > MAX_PASTE_BYTES || text.as_bytes().contains(&0) {
            return false;
        }
        self.endpoint.insert_text(origin, text)
    }
}

struct OperationReservation<'a> {
    identity: &'a ServiceOperationIdentity,
    completed: bool,
}

impl Drop for OperationReservation<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.identity.release_return();
        }
    }
}

struct ServiceOperationIdentity {
    request_id: u64,
    returns_text: bool,
    active_request: Rc<Cell<Option<u64>>>,
    write_claimed: Cell<bool>,
    pasteboard: Cell<Option<ServicePasteboardIdentity>>,
    origin: Cell<Option<NativeServiceOrigin>>,
}

impl ServiceOperationIdentity {
    fn claim_write(&self) -> bool {
        if self.write_claimed.replace(true) || self.active_request.get().is_some() {
            return false;
        }
        // Reserve send-only requests too until publication finishes, preventing a reentrant
        // transform from replacing the same pasteboard while its selection is being written.
        self.active_request.set(Some(self.request_id));
        true
    }

    fn finish_send(
        &self,
        origin: NativeServiceOrigin,
        pasteboard: ServicePasteboardIdentity,
        succeeded: bool,
    ) {
        if self.returns_text && succeeded {
            self.origin.set(Some(origin));
            self.pasteboard.set(Some(pasteboard));
        } else {
            self.release_return();
        }
    }

    fn take_return_origin(
        &self,
        pasteboard: ServicePasteboardIdentity,
    ) -> Option<NativeServiceOrigin> {
        if self.active_request.get() != Some(self.request_id)
            || self.pasteboard.get() != Some(pasteboard)
        {
            return None;
        }
        let origin = self.origin.take();
        self.pasteboard.set(None);
        origin
    }

    fn release_return(&self) {
        if self.active_request.get() == Some(self.request_id) {
            self.active_request.set(None);
        }
    }
}

impl Drop for ServiceOperationIdentity {
    fn drop(&mut self) {
        self.release_return();
    }
}

fn accepts_service_request(
    capabilities: NativeServiceCapabilities,
    send_type: ServiceDataType,
    return_type: ServiceDataType,
) -> bool {
    if !send_type.is_supported() || !return_type.is_supported() {
        return false;
    }
    if !send_type.is_requested() && !return_type.is_requested() {
        return false;
    }
    if return_type.is_requested() && !send_type.is_requested() {
        return false;
    }
    (!send_type.is_requested() || capabilities.send_text)
        && (!return_type.is_requested() || capabilities.return_text)
}

pub(crate) fn bounded_service_text_byte_len(
    character_len: usize,
    byte_len: usize,
) -> Option<usize> {
    ((byte_len != 0 || character_len == 0) && byte_len <= MAX_PASTE_BYTES).then_some(byte_len)
}

pub(crate) fn decode_service_text_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_PASTE_BYTES || bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PaneId, TabId, WorkspaceId};
    fn origin(generation: u64) -> NativeServiceOrigin {
        NativeServiceOrigin::new(
            WorkspaceId::new(1),
            TabId::new(2),
            PaneId::new(3),
            4,
            5,
            generation,
        )
    }

    fn operation_identity(
        request_id: u64,
        returns_text: bool,
        active_request: Rc<Cell<Option<u64>>>,
    ) -> ServiceOperationIdentity {
        ServiceOperationIdentity {
            request_id,
            returns_text,
            active_request,
            write_claimed: Cell::new(false),
            pasteboard: Cell::new(None),
            origin: Cell::new(None),
        }
    }

    #[test]
    fn service_request_requires_each_requested_capability() {
        assert!(accepts_service_request(
            NativeServiceCapabilities::new(true, false),
            ServiceDataType::String,
            ServiceDataType::Absent,
        ));
        assert!(!accepts_service_request(
            NativeServiceCapabilities::new(true, false),
            ServiceDataType::String,
            ServiceDataType::String,
        ));
    }

    #[test]
    fn string_return_requires_a_bound_send_and_terminal_input_focus_capability() {
        assert!(accepts_service_request(
            NativeServiceCapabilities::new(true, true),
            ServiceDataType::String,
            ServiceDataType::String,
        ));
        assert!(!accepts_service_request(
            NativeServiceCapabilities::new(true, true),
            ServiceDataType::Absent,
            ServiceDataType::String,
        ));
    }

    #[test]
    fn unsupported_service_types_are_never_accepted() {
        assert!(!accepts_service_request(
            NativeServiceCapabilities::new(true, true),
            ServiceDataType::Unsupported,
            ServiceDataType::Absent,
        ));
    }

    #[test]
    fn service_operation_keeps_the_validated_origin_through_send_and_return() {
        let operation = operation_identity(1, true, Rc::new(Cell::new(None)));
        let pasteboard = ServicePasteboardIdentity::new(0x1234);

        assert!(operation.claim_write());
        operation.finish_send(origin(6), pasteboard, true);

        assert_eq!(operation.take_return_origin(pasteboard), Some(origin(6)));
        assert_eq!(operation.take_return_origin(pasteboard), None);
    }

    #[test]
    fn service_operation_rejects_overlap_and_wrong_pasteboard_return() {
        let active = Rc::new(Cell::new(None));
        let operation = operation_identity(1, true, Rc::clone(&active));
        let overlap = operation_identity(2, true, active);
        let first_pasteboard = ServicePasteboardIdentity::new(0x1234);
        let other_pasteboard = ServicePasteboardIdentity::new(0x5678);
        assert!(operation.claim_write());
        operation.finish_send(origin(6), first_pasteboard, true);

        assert!(!overlap.claim_write());
        assert_eq!(overlap.take_return_origin(first_pasteboard), None);
        assert_eq!(operation.take_return_origin(other_pasteboard), None);
        assert_eq!(
            operation.take_return_origin(first_pasteboard),
            Some(origin(6))
        );
    }

    #[test]
    fn repeated_validation_produces_distinct_operation_identities() {
        let active = Rc::new(Cell::new(None));
        let transform = operation_identity(1, true, Rc::clone(&active));
        let send_only = operation_identity(2, false, active);

        assert_ne!(transform.request_id, send_only.request_id);
        assert!(transform.returns_text);
        assert!(!send_only.returns_text);
    }

    #[test]
    fn service_operation_write_is_one_shot() {
        let operation = operation_identity(1, false, Rc::new(Cell::new(None)));

        assert!(operation.claim_write());
        assert!(!operation.claim_write());
    }

    #[test]
    fn return_only_service_cannot_create_an_unbound_operation() {
        assert!(!accepts_service_request(
            NativeServiceCapabilities::new(true, true),
            ServiceDataType::Absent,
            ServiceDataType::String,
        ));
    }

    #[test]
    fn service_text_rejects_oversized_and_embedded_nul_payloads() {
        assert!(decode_service_text_bytes(&vec![b'x'; MAX_PASTE_BYTES + 1]).is_none());
        assert!(decode_service_text_bytes(b"before\0after").is_none());
        assert_eq!(
            decode_service_text_bytes("日本語".as_bytes()).as_deref(),
            Some("日本語")
        );
        assert_eq!(bounded_service_text_byte_len(1, 0), None);
    }

    struct Endpoint {
        status: Cell<NativeServiceStatus>,
        calls: Cell<usize>,
        inserted: std::cell::RefCell<Vec<(NativeServiceOrigin, String)>>,
    }

    impl Endpoint {
        fn new() -> Rc<Self> {
            Rc::new(Self {
                status: Cell::new(NativeServiceStatus::new(
                    NativeServiceCapabilities::new(true, true),
                    Some(origin(6)),
                )),
                calls: Cell::new(0),
                inserted: std::cell::RefCell::new(Vec::new()),
            })
        }
    }

    impl ServiceEndpoint for Endpoint {
        fn status(&self) -> NativeServiceStatus {
            self.calls.set(self.calls.get() + 1);
            self.status.get()
        }

        fn selection(&self, requested: NativeServiceOrigin) -> Option<SelectionCopy> {
            self.calls.set(self.calls.get() + 1);
            (self.status.get().origin == Some(requested)).then(|| SelectionCopy {
                plain_text: "selected".to_owned(),
                html: None,
            })
        }

        fn insert_text(&self, requested: NativeServiceOrigin, text: String) -> bool {
            self.calls.set(self.calls.get() + 1);
            let status = self.status.get();
            if status.origin != Some(requested) || !status.capabilities.return_text {
                return false;
            }
            self.inserted.borrow_mut().push((requested, text));
            true
        }
    }

    fn transform(requests: &ServiceRequests) -> ServiceOperation {
        requests
            .operation(ServiceDataType::String, ServiceDataType::String)
            .unwrap()
    }

    #[test]
    fn requests_allocate_distinct_identities_and_refuse_exhaustion() {
        let requests = ServiceRequests::new(Endpoint::new());
        let first = transform(&requests);
        let second = transform(&requests);
        assert_ne!(first.identity.request_id, second.identity.request_id);
        requests.next_request_id.set(u64::MAX);
        assert!(
            requests
                .operation(ServiceDataType::String, ServiceDataType::String)
                .is_none()
        );
    }

    #[test]
    fn synchronous_send_and_one_shot_return_reach_only_the_captured_endpoint() {
        let first = Endpoint::new();
        let second = Endpoint::new();
        let first_requests = ServiceRequests::new(first.clone());
        let second_requests = ServiceRequests::new(second.clone());
        let operation = transform(&first_requests);
        let board = ServicePasteboardIdentity::new(1);
        let published = Cell::new(false);
        assert!(operation.write_selection(board, |text| {
            assert_eq!(text, "selected");
            published.set(true);
            true
        }));
        assert!(published.get());
        // Identical Pane origins and pasteboard tokens in another window confer no authority.
        assert!(!transform(&second_requests).read_selection(board, || Some("wrong".into())));
        assert!(operation.read_selection(board, || Some("returned".into())));
        assert!(!operation.read_selection(board, || panic!("duplicate return was read")));
        assert_eq!(
            *first.inserted.borrow(),
            vec![(origin(6), "returned".into())]
        );
        assert!(second.inserted.borrow().is_empty());
    }

    #[test]
    fn focus_or_hierarchy_change_rejects_return_and_consumes_authority() {
        let endpoint = Endpoint::new();
        let requests = ServiceRequests::new(endpoint.clone());
        let operation = transform(&requests);
        let board = ServicePasteboardIdentity::new(1);
        assert!(operation.write_selection(board, |_| true));
        endpoint.status.set(NativeServiceStatus::new(
            NativeServiceCapabilities::new(true, true),
            Some(origin(7)),
        ));
        assert!(!operation.read_selection(board, || Some("stale".into())));
        endpoint.status.set(NativeServiceStatus::new(
            NativeServiceCapabilities::new(true, true),
            Some(origin(6)),
        ));
        assert!(!operation.read_selection(board, || panic!("stale return retried")));
        assert!(endpoint.inserted.borrow().is_empty());
    }

    #[test]
    fn callback_reentrancy_cannot_overlap_send_or_duplicate_return() {
        let requests = ServiceRequests::new(Endpoint::new());
        let operation = requests
            .operation(ServiceDataType::String, ServiceDataType::Absent)
            .unwrap();
        let board = ServicePasteboardIdentity::new(1);
        assert!(operation.write_selection(board, |_| {
            assert!(!transform(&requests).write_selection(board, |_| panic!("overlapping write")));
            true
        }));
        let operation = transform(&requests);
        assert!(operation.write_selection(board, |_| true));
        assert!(operation.read_selection(board, || {
            assert!(!operation.read_selection(board, || panic!("reentrant return")));
            assert!(
                !transform(&requests)
                    .write_selection(board, |_| panic!("reentrant return overwrite"))
            );
            Some("returned".into())
        }));
    }

    #[test]
    fn failed_or_panicked_send_releases_reservation_before_responder_destruction() {
        let requests = ServiceRequests::new(Endpoint::new());
        let board = ServicePasteboardIdentity::new(1);
        let failed = transform(&requests);
        assert!(!failed.write_selection(board, |_| false));
        let panicked = transform(&requests);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                panicked.write_selection(board, |_| panic!("injected native write failure"))
            }))
            .is_err()
        );
        let next = transform(&requests);
        assert!(next.write_selection(board, |_| true));
        drop(failed);
        drop(panicked);
        assert!(next.read_selection(board, || Some("returned".into())));
    }

    #[test]
    fn dropping_pending_operation_releases_only_its_own_reservation() {
        let requests = ServiceRequests::new(Endpoint::new());
        let board = ServicePasteboardIdentity::new(1);
        let first = transform(&requests);
        assert!(first.write_selection(board, |_| true));
        let rejected = transform(&requests);
        assert!(!rejected.write_selection(board, |_| true));
        drop(rejected);
        assert!(first.read_selection(board, || Some("returned".into())));
        let cancelled = transform(&requests);
        assert!(cancelled.write_selection(board, |_| true));
        drop(cancelled);
        assert!(transform(&requests).write_selection(board, |_| true));
    }

    #[test]
    fn invalid_return_is_consumed_without_calling_endpoint() {
        let endpoint = Endpoint::new();
        let requests = ServiceRequests::new(endpoint.clone());
        let board = ServicePasteboardIdentity::new(1);
        for text in [
            None,
            Some("bad\0text".into()),
            Some("x".repeat(MAX_PASTE_BYTES + 1)),
        ] {
            let operation = transform(&requests);
            assert!(operation.write_selection(board, |_| true));
            assert!(!operation.read_selection(board, || text));
            assert!(!operation.read_selection(board, || panic!("invalid return retried")));
        }
        assert!(endpoint.inserted.borrow().is_empty());
    }

    #[test]
    fn validation_rejects_missing_origin_and_every_unsupported_contract() {
        let endpoint = Endpoint::new();
        let requests = ServiceRequests::new(endpoint.clone());
        let kinds = [
            ServiceDataType::Absent,
            ServiceDataType::String,
            ServiceDataType::Unsupported,
        ];
        for send in kinds {
            for returned in kinds {
                let accepted = requests.operation(send, returned).is_some();
                assert_eq!(
                    accepted,
                    send == ServiceDataType::String && returned != ServiceDataType::Unsupported
                );
            }
        }
        let allocated = requests.next_request_id.get();
        endpoint.status.set(NativeServiceStatus::new(
            NativeServiceCapabilities::new(true, true),
            None,
        ));
        assert!(
            requests
                .operation(ServiceDataType::String, ServiceDataType::String)
                .is_none()
        );
        assert_eq!(requests.next_request_id.get(), allocated);
    }

    #[test]
    fn retained_validation_cannot_export_successor_selection() {
        let endpoint = Endpoint::new();
        let requests = ServiceRequests::new(endpoint.clone());
        let stale = transform(&requests);
        endpoint.status.set(NativeServiceStatus::new(
            NativeServiceCapabilities::new(true, true),
            Some(origin(7)),
        ));
        let board = ServicePasteboardIdentity::new(1);
        assert!(!stale.write_selection(board, |_| panic!("successor selection exported")));
        let successor = transform(&requests);
        assert!(successor.write_selection(board, |_| true));
        assert!(!stale.read_selection(board, || panic!("stale operation read successor return")));
        assert!(successor.read_selection(board, || Some("successor return".into())));
        assert_eq!(
            *endpoint.inserted.borrow(),
            vec![(origin(7), "successor return".into())]
        );
    }

    #[test]
    fn owner_teardown_revokes_retained_sends_and_returns_before_any_callback() {
        let endpoint = Endpoint::new();
        let requests = ServiceRequests::new(endpoint.clone());
        let pending_send = transform(&requests);
        let pending_return = transform(&requests);
        let board = ServicePasteboardIdentity::new(1);
        assert!(pending_return.write_selection(board, |_| true));
        let calls = endpoint.calls.get();
        drop(requests);
        assert!(!pending_send.write_selection(board, |_| panic!("write after owner teardown")));
        assert!(!pending_return.read_selection(board, || panic!("read after owner teardown")));
        assert_eq!(endpoint.calls.get(), calls);
        assert!(endpoint.inserted.borrow().is_empty());
    }

    #[test]
    fn owner_teardown_during_native_return_prevents_endpoint_insertion() {
        let endpoint = Endpoint::new();
        let requests = ServiceRequests::new(endpoint.clone());
        let operation = transform(&requests);
        let board = ServicePasteboardIdentity::new(1);
        assert!(operation.write_selection(board, |_| true));
        assert!(!operation.read_selection(board, || {
            drop(requests);
            Some("returned after teardown".into())
        }));
        assert!(endpoint.inserted.borrow().is_empty());
    }

    #[test]
    fn owner_teardown_during_native_send_cannot_arm_a_return() {
        let endpoint = Endpoint::new();
        let requests = ServiceRequests::new(endpoint.clone());
        let operation = transform(&requests);
        let board = ServicePasteboardIdentity::new(1);
        assert!(!operation.write_selection(board, |_| {
            drop(requests);
            true
        }));
        assert!(!operation.read_selection(board, || panic!("return armed after teardown")));
        assert!(endpoint.inserted.borrow().is_empty());
    }

    #[test]
    fn panicked_return_releases_reservation_and_remains_consumed() {
        let requests = ServiceRequests::new(Endpoint::new());
        let operation = transform(&requests);
        let board = ServicePasteboardIdentity::new(1);
        assert!(operation.write_selection(board, |_| true));
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                operation.read_selection(board, || panic!("injected native read failure"))
            }))
            .is_err()
        );
        assert!(!operation.read_selection(board, || panic!("panicked return retried")));
        assert!(transform(&requests).write_selection(board, |_| true));
    }

    #[test]
    fn native_retirement_revokes_operations_even_while_callback_retains_owner() {
        let endpoint = Endpoint::new();
        let requests = Rc::new(ServiceRequests::new(endpoint.clone()));
        let callback_owner = Rc::clone(&requests);
        let operation = transform(&requests);
        let board = ServicePasteboardIdentity::new(1);
        assert!(operation.write_selection(board, |_| true));
        let calls = endpoint.calls.get();
        requests.retire();
        drop(requests);
        assert!(callback_owner.is_retired());
        assert!(
            callback_owner
                .operation(ServiceDataType::String, ServiceDataType::String)
                .is_none()
        );
        assert!(!operation.read_selection(board, || panic!("native return after retirement")));
        assert_eq!(endpoint.calls.get(), calls);
    }
}
