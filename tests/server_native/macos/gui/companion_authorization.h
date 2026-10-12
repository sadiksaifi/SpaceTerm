#ifndef SPACETERM_EVIDENCE_COMPANION_AUTHORIZATION_H
#define SPACETERM_EVIDENCE_COMPANION_AUTHORIZATION_H

#include "companion_model.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/*
 * Consumer side of the management job's fixed GUI adapter.
 *
 * The companion never authorizes or executes anything itself. It asks the
 * port for a current assessment, shows its own confirmation for destructive
 * operations, and only then hands the confirmed assessment to the port. The
 * port's execute call is where the management adapter requests the
 * operation-specific Authorization Services right and macOS shows its own
 * prompt. The two steps are never merged: confirming here does not authorize,
 * and authorizing in macOS does not skip confirmation here.
 *
 * Port functions return 0 or a NativeFailure repr(u32) value. NativeUnavailable
 * from a linked adapter means macOS lacks a service the operation needs, so it
 * is shown as NATIVE_UNAVAILABLE. Until the coordinator binds the management
 * adapter, the production composition uses
 * st_companion_authorization_unavailable(). The flow recognizes that port, and
 * any port with a missing function, finishes as ADAPTER_UNAVAILABLE, and never
 * calls it. There is no fallback route.
 */

enum {
    ST_COMPANION_ASSESSMENT_SECONDS = 30,
    ST_COMPANION_BINDING_BYTES = 32,
    ST_COMPANION_REQUEST_BYTES = 16,
    ST_COMPANION_MAX_REASSESSMENTS = 3,
    ST_COMPANION_MAX_CONFIRMATION = 320
};

/* Operation codes match the management adapter's Start, Stop, and Restart. */
typedef enum {
    ST_COMPANION_OPERATION_START = 1,
    ST_COMPANION_OPERATION_STOP = 2,
    ST_COMPANION_OPERATION_RESTART = 3
} st_companion_operation;

enum { ST_COMPANION_OPERATION_COUNT = 3 };

/* A bounded, content-free view of NativeAssessment. */
typedef struct {
    uint32_t operation;
    /* Terminal Sessions the operation would end. */
    uint32_t terminal_sessions;
    /* Git writes that already started and finish before the operation. */
    uint32_t started_writes;
    uint32_t reserved;
    /* Opaque binding the adapter uses to recognize this exact assessment. */
    uint8_t binding[ST_COMPANION_BINDING_BYTES];
} st_companion_assessment;

typedef enum {
    ST_COMPANION_COMPLETION_RUNNING = 1,
    ST_COMPANION_COMPLETION_STOPPED = 2,
    ST_COMPANION_COMPLETION_PENDING = 3
} st_companion_completion;

/* A bounded, content-free view of NativeProgress. */
typedef struct {
    uint32_t completion;
    uint32_t reserved;
    uint8_t request[ST_COMPANION_REQUEST_BYTES];
} st_companion_execution;

typedef struct {
    void *context;
    uint32_t (*assess)(void *context, uint32_t operation, st_companion_assessment *out);
    uint32_t (*execute)(void *context,
                        const st_companion_assessment *confirmed,
                        st_companion_execution *out);
} st_companion_authorization_port;

/* The production binding until the management adapter is linked. Its
 * functions report NativeUnavailable to any direct caller. */
const st_companion_authorization_port *st_companion_authorization_unavailable(void);

typedef enum {
    ST_COMPANION_AUTH_IDLE = 0,
    ST_COMPANION_AUTH_CONFIRMING = 1,
    ST_COMPANION_AUTH_AUTHORIZING = 2,
    ST_COMPANION_AUTH_FINISHED = 3
} st_companion_auth_phase;

typedef enum {
    ST_COMPANION_AUTH_OUTCOME_NONE = 0,
    ST_COMPANION_AUTH_OUTCOME_COMPLETED_RUNNING = 1,
    ST_COMPANION_AUTH_OUTCOME_COMPLETED_STOPPED = 2,
    ST_COMPANION_AUTH_OUTCOME_PENDING = 3,
    /* The person declined the companion's confirmation. macOS never asked. */
    ST_COMPANION_AUTH_OUTCOME_DECLINED = 4,
    ST_COMPANION_AUTH_OUTCOME_DENIED = 5,
    ST_COMPANION_AUTH_OUTCOME_CANCELLED = 6,
    /* The adapter could not establish an Authorization Services route. */
    ST_COMPANION_AUTH_OUTCOME_UNAVAILABLE = 7,
    /* No management adapter is linked into this companion. Decided by the
     * composition, never by a failure code. */
    ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE = 8,
    ST_COMPANION_AUTH_OUTCOME_BACKGROUND_ITEMS_SUPPRESSED = 9,
    ST_COMPANION_AUTH_OUTCOME_TIMED_OUT = 10,
    ST_COMPANION_AUTH_OUTCOME_STALE = 11,
    ST_COMPANION_AUTH_OUTCOME_INDETERMINATE = 12,
    ST_COMPANION_AUTH_OUTCOME_CLEANUP_UNRESOLVED = 13,
    ST_COMPANION_AUTH_OUTCOME_FAILED = 14,
    /* The linked adapter reported NativeUnavailable: macOS lacks a service. */
    ST_COMPANION_AUTH_OUTCOME_NATIVE_UNAVAILABLE = 15
} st_companion_auth_outcome;

enum { ST_COMPANION_AUTH_OUTCOME_COUNT = 16 };

typedef struct {
    st_companion_auth_phase phase;
    st_companion_operation operation;
    st_companion_assessment assessment;
    uint64_t assessed_ns;
    uint32_t reassessments;
    /* How many times the port's execute was called for this flow. */
    uint32_t executions;
    st_companion_auth_outcome outcome;
    /* NativeFailure value behind the outcome, or 0. */
    uint32_t failure;
    st_companion_execution execution;
} st_companion_auth_flow;

const char *st_companion_operation_token(st_companion_operation operation);
/* "Start Server", "Stop Server", or "Restart Server". */
const char *st_companion_operation_title(st_companion_operation operation);
/* Stop and Restart end Terminal Sessions and need the companion's confirmation. */
bool st_companion_operation_is_destructive(st_companion_operation operation);

const char *st_companion_auth_outcome_token(st_companion_auth_outcome outcome);
st_companion_status_copy st_companion_auth_outcome_copy(st_companion_auth_outcome outcome);
/* Maps a nonzero port result to an outcome. */
st_companion_auth_outcome st_companion_auth_outcome_for_failure(uint32_t failure);

void st_companion_auth_init(st_companion_auth_flow *flow);

/* Starts a flow from IDLE or FINISHED and calls port->assess once. Moves to
 * CONFIRMING for destructive operations, AUTHORIZING for Start, or FINISHED
 * when assessment fails. */
st_companion_auth_phase st_companion_auth_begin(st_companion_auth_flow *flow,
                                                const st_companion_authorization_port *port,
                                                st_companion_operation operation,
                                                uint64_t now_ns);

/* Records the person's answer to the companion's own confirmation. Declining
 * finishes as DECLINED without calling the port. Confirming an assessment
 * older than ST_COMPANION_ASSESSMENT_SECONDS assesses again and stays in
 * CONFIRMING so the person confirms the current facts. Otherwise moves to
 * AUTHORIZING. Never calls port->execute. */
st_companion_auth_phase st_companion_auth_confirm(st_companion_auth_flow *flow,
                                                  const st_companion_authorization_port *port,
                                                  bool confirmed,
                                                  uint64_t now_ns);

/* From AUTHORIZING only: calls port->execute once with the confirmed
 * assessment and finishes. */
st_companion_auth_phase st_companion_auth_execute(st_companion_auth_flow *flow,
                                                  const st_companion_authorization_port *port,
                                                  uint64_t now_ns);

/* The companion's confirmation for destructive operations. Writes a
 * NUL-terminated message with the assessed counts and returns its length, or
 * 0 when the flow is not CONFIRMING or capacity is short. */
size_t st_companion_auth_confirmation_message(const st_companion_auth_flow *flow,
                                              char *out,
                                              size_t capacity);
const char *st_companion_auth_confirmation_title(const st_companion_auth_flow *flow);
const char *st_companion_auth_confirmation_button(const st_companion_auth_flow *flow);

#endif
