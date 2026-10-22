# Negotiate one initial protocol major before the first release

SpaceTerm uses one initial SpaceTerm Protocol major, major 1, with negotiated optional capabilities and refusal of incompatible peers before operations. No server/client architecture has shipped. Historical development majors 1 and 2 create no compatibility commitment, so unreleased formats are replaced directly without old-format readers, dual authentication or migration frameworks.

Once released major history exists, each client and server release supports the current and previous released major, choosing the newest shared version so devices can update independently. Breaking changes introduce a new major; optional additions use capabilities. Incompatibility follows [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md).

Admission checks both the shared protocol major and the client's required capabilities before reporting a server ready or admitting dependent operations, including account-local startup. A missing optional capability disables only the feature that requires it. Client and server release numbers may differ without incompatibility; refusal of one connection does not change another compatible server or its work.

A reset of incompatible unreleased state requires explicit confirmation of its exact scope and loss. Started work must first complete or reconcile, and unresolved process cleanup refuses reset. Preserve shipped Settings schema 7, serialized Command IDs, released user data, deliberate Stop, SSH trust and unrelated credentials and signing keys. Optional cleanup of exact obsolete local credential items never gates local use.
