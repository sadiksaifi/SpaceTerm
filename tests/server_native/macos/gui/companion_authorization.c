#include "companion_authorization.h"

#include <stdio.h>
#include <string.h>

/* NativeFailure values the flow distinguishes. */
enum {
    FAILURE_NATIVE_UNAVAILABLE = 2,
    FAILURE_STALE_ASSESSMENT = 14,
    FAILURE_AUTHORIZATION_DENIED = 16,
    FAILURE_AUTHORIZATION_CANCELLED = 17,
    FAILURE_AUTHORIZATION_UNAVAILABLE = 18,
    FAILURE_BACKGROUND_ITEMS_SUPPRESSED = 25,
    FAILURE_TIMED_OUT = 29,
    FAILURE_OUTCOME_PENDING = 31,
    FAILURE_INDETERMINATE = 32,
    FAILURE_CLEANUP_UNRESOLVED = 33
};

static uint32_t unavailable_assess(void *context, uint32_t operation,
                                   st_companion_assessment *out)
{
    (void)context;
    (void)operation;
    (void)out;
    return FAILURE_NATIVE_UNAVAILABLE;
}

static uint32_t unavailable_execute(void *context, const st_companion_assessment *confirmed,
                                    st_companion_execution *out)
{
    (void)context;
    (void)confirmed;
    (void)out;
    return FAILURE_NATIVE_UNAVAILABLE;
}

const st_companion_authorization_port *st_companion_authorization_unavailable(void)
{
    static const st_companion_authorization_port port = {NULL, unavailable_assess,
                                                         unavailable_execute};
    return &port;
}

const char *st_companion_operation_token(st_companion_operation operation)
{
    switch (operation) {
    case ST_COMPANION_OPERATION_START: return "start";
    case ST_COMPANION_OPERATION_STOP: return "stop";
    case ST_COMPANION_OPERATION_RESTART: return "restart";
    }
    return NULL;
}

const char *st_companion_operation_title(st_companion_operation operation)
{
    switch (operation) {
    case ST_COMPANION_OPERATION_START: return "Start Server";
    case ST_COMPANION_OPERATION_STOP: return "Stop Server";
    case ST_COMPANION_OPERATION_RESTART: return "Restart Server";
    }
    return NULL;
}

bool st_companion_operation_is_destructive(st_companion_operation operation)
{
    return operation == ST_COMPANION_OPERATION_STOP || operation == ST_COMPANION_OPERATION_RESTART;
}

static const char *const outcome_tokens[ST_COMPANION_AUTH_OUTCOME_COUNT] = {
    "none",
    "completed-running",
    "completed-stopped",
    "pending",
    "declined",
    "denied",
    "cancelled",
    "unavailable",
    "adapter-unavailable",
    "background-items-suppressed",
    "timed-out",
    "stale",
    "indeterminate",
    "cleanup-unresolved",
    "failed",
    "native-unavailable",
};

static const st_companion_status_copy outcome_copy[ST_COMPANION_AUTH_OUTCOME_COUNT] = {
    {"No request", "No authorization has been requested yet.", "circle.dashed",
     ST_COMPANION_TONE_NEUTRAL},
    {"Running", "SpaceTerm Server reported that it is running.", "checkmark.circle",
     ST_COMPANION_TONE_POSITIVE},
    {"Stopped", "SpaceTerm Server reported that it stopped.", "checkmark.circle",
     ST_COMPANION_TONE_POSITIVE},
    {"Pending",
     "SpaceTerm Server accepted the request and has not finished it. Its result is not known "
     "yet.",
     "hourglass", ST_COMPANION_TONE_PROGRESS},
    {"Not confirmed", "You declined the confirmation, so macOS was not asked for authorization.",
     "arrow.uturn.backward.circle", ST_COMPANION_TONE_NEUTRAL},
    {"Authorization denied", "macOS denied authorization. Nothing was changed.",
     "xmark.shield", ST_COMPANION_TONE_NEGATIVE},
    {"Authorization cancelled", "The macOS authorization prompt was cancelled. Nothing was changed.",
     "minus.circle", ST_COMPANION_TONE_CAUTION},
    {"Authorization unavailable",
     "macOS authorization could not be used for this request. Nothing was changed.",
     "slash.circle", ST_COMPANION_TONE_CAUTION},
    {"Adapter unavailable",
     "The management adapter is not available, so the companion could not request "
     "authorization. Nothing was changed.",
     "puzzlepiece.extension", ST_COMPANION_TONE_CAUTION},
    {"Background item off",
     "macOS reports that SpaceTerm Server's background item is turned off in Login Items & "
     "Extensions. The companion does not change this setting.",
     "exclamationmark.triangle", ST_COMPANION_TONE_CAUTION},
    {"Timed out", "The request did not finish in time. Check the server's state before retrying.",
     "clock.badge.exclamationmark", ST_COMPANION_TONE_CAUTION},
    {"Assessment expired",
     "The assessment expired before the change began. Request authorization again to assess "
     "the current state.",
     "clock.arrow.circlepath", ST_COMPANION_TONE_CAUTION},
    {"Indeterminate",
     "The result could not be established. SpaceTerm Server accepts no further changes until "
     "recovery checks its state.",
     "questionmark.circle", ST_COMPANION_TONE_CAUTION},
    {"Cleanup unresolved",
     "Some cleanup could not be confirmed. Recorded resources remain for recovery.",
     "exclamationmark.arrow.circlepath", ST_COMPANION_TONE_NEGATIVE},
    {"Failed", "The request failed. The failure name is shown with this status.",
     "exclamationmark.triangle", ST_COMPANION_TONE_NEGATIVE},
    {"macOS service unavailable",
     "The management adapter reported that a macOS service this operation needs is not "
     "available. Check the server's state before retrying.",
     "exclamationmark.octagon", ST_COMPANION_TONE_CAUTION},
};

const char *st_companion_auth_outcome_token(st_companion_auth_outcome outcome)
{
    if ((unsigned)outcome >= ST_COMPANION_AUTH_OUTCOME_COUNT) return "failed";
    return outcome_tokens[outcome];
}

st_companion_status_copy st_companion_auth_outcome_copy(st_companion_auth_outcome outcome)
{
    if ((unsigned)outcome >= ST_COMPANION_AUTH_OUTCOME_COUNT) {
        return outcome_copy[ST_COMPANION_AUTH_OUTCOME_FAILED];
    }
    return outcome_copy[outcome];
}

st_companion_auth_outcome st_companion_auth_outcome_for_failure(uint32_t failure)
{
    switch (failure) {
    case FAILURE_NATIVE_UNAVAILABLE: return ST_COMPANION_AUTH_OUTCOME_NATIVE_UNAVAILABLE;
    case FAILURE_STALE_ASSESSMENT: return ST_COMPANION_AUTH_OUTCOME_STALE;
    case FAILURE_AUTHORIZATION_DENIED: return ST_COMPANION_AUTH_OUTCOME_DENIED;
    case FAILURE_AUTHORIZATION_CANCELLED: return ST_COMPANION_AUTH_OUTCOME_CANCELLED;
    case FAILURE_AUTHORIZATION_UNAVAILABLE: return ST_COMPANION_AUTH_OUTCOME_UNAVAILABLE;
    case FAILURE_BACKGROUND_ITEMS_SUPPRESSED:
        return ST_COMPANION_AUTH_OUTCOME_BACKGROUND_ITEMS_SUPPRESSED;
    case FAILURE_TIMED_OUT: return ST_COMPANION_AUTH_OUTCOME_TIMED_OUT;
    case FAILURE_OUTCOME_PENDING: return ST_COMPANION_AUTH_OUTCOME_PENDING;
    case FAILURE_INDETERMINATE: return ST_COMPANION_AUTH_OUTCOME_INDETERMINATE;
    case FAILURE_CLEANUP_UNRESOLVED: return ST_COMPANION_AUTH_OUTCOME_CLEANUP_UNRESOLVED;
    default: return ST_COMPANION_AUTH_OUTCOME_FAILED;
    }
}

void st_companion_auth_init(st_companion_auth_flow *flow)
{
    memset(flow, 0, sizeof *flow);
}

static st_companion_auth_phase finish(st_companion_auth_flow *flow,
                                      st_companion_auth_outcome outcome,
                                      uint32_t failure)
{
    flow->phase = ST_COMPANION_AUTH_FINISHED;
    flow->outcome = outcome;
    flow->failure = failure;
    return flow->phase;
}

/* False when no management adapter is linked. */
static bool port_usable(const st_companion_authorization_port *port)
{
    return port != NULL && port != st_companion_authorization_unavailable() &&
           port->assess != NULL && port->execute != NULL;
}

/* Calls assess once. Returns false after finishing the flow on failure. */
static bool assess(st_companion_auth_flow *flow, const st_companion_authorization_port *port,
                   uint64_t now_ns)
{
    st_companion_assessment assessment;
    memset(&assessment, 0, sizeof assessment);
    uint32_t failure = port->assess(port->context, (uint32_t)flow->operation, &assessment);
    if (failure != 0) {
        (void)finish(flow, st_companion_auth_outcome_for_failure(failure), failure);
        return false;
    }
    if (assessment.operation != (uint32_t)flow->operation) {
        /* The adapter answered for another operation; nothing can proceed. */
        (void)finish(flow, ST_COMPANION_AUTH_OUTCOME_INDETERMINATE, 0);
        return false;
    }
    flow->assessment = assessment;
    flow->assessed_ns = now_ns;
    return true;
}

static bool stale(const st_companion_auth_flow *flow, uint64_t now_ns)
{
    const uint64_t limit = (uint64_t)ST_COMPANION_ASSESSMENT_SECONDS * UINT64_C(1000000000);
    return now_ns < flow->assessed_ns || now_ns - flow->assessed_ns > limit;
}

/* Replaces an expired assessment, bounded so a slow person cannot loop. */
static bool reassess(st_companion_auth_flow *flow, const st_companion_authorization_port *port,
                     uint64_t now_ns)
{
    if (flow->reassessments >= ST_COMPANION_MAX_REASSESSMENTS) {
        (void)finish(flow, ST_COMPANION_AUTH_OUTCOME_STALE, FAILURE_STALE_ASSESSMENT);
        return false;
    }
    ++flow->reassessments;
    return assess(flow, port, now_ns);
}

st_companion_auth_phase st_companion_auth_begin(st_companion_auth_flow *flow,
                                                const st_companion_authorization_port *port,
                                                st_companion_operation operation,
                                                uint64_t now_ns)
{
    if (flow->phase == ST_COMPANION_AUTH_CONFIRMING ||
        flow->phase == ST_COMPANION_AUTH_AUTHORIZING) {
        return flow->phase;
    }
    st_companion_auth_init(flow);
    flow->operation = operation;
    if (st_companion_operation_token(operation) == NULL) {
        return finish(flow, ST_COMPANION_AUTH_OUTCOME_FAILED, 0);
    }
    if (!port_usable(port)) {
        return finish(flow, ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE, 0);
    }
    if (!assess(flow, port, now_ns)) return flow->phase;
    flow->phase = st_companion_operation_is_destructive(operation) ? ST_COMPANION_AUTH_CONFIRMING
                                                                   : ST_COMPANION_AUTH_AUTHORIZING;
    return flow->phase;
}

st_companion_auth_phase st_companion_auth_confirm(st_companion_auth_flow *flow,
                                                  const st_companion_authorization_port *port,
                                                  bool confirmed,
                                                  uint64_t now_ns)
{
    if (flow->phase != ST_COMPANION_AUTH_CONFIRMING) return flow->phase;
    if (!confirmed) return finish(flow, ST_COMPANION_AUTH_OUTCOME_DECLINED, 0);
    if (!port_usable(port)) {
        return finish(flow, ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE, 0);
    }
    if (stale(flow, now_ns)) {
        /* The person confirmed facts that may have changed: show the new ones. */
        (void)reassess(flow, port, now_ns);
        return flow->phase;
    }
    flow->phase = ST_COMPANION_AUTH_AUTHORIZING;
    return flow->phase;
}

st_companion_auth_phase st_companion_auth_execute(st_companion_auth_flow *flow,
                                                  const st_companion_authorization_port *port,
                                                  uint64_t now_ns)
{
    if (flow->phase != ST_COMPANION_AUTH_AUTHORIZING) return flow->phase;
    if (!port_usable(port)) {
        return finish(flow, ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE, 0);
    }
    if (stale(flow, now_ns)) {
        if (!reassess(flow, port, now_ns)) return flow->phase;
        if (st_companion_operation_is_destructive(flow->operation)) {
            flow->phase = ST_COMPANION_AUTH_CONFIRMING;
            return flow->phase;
        }
    }
    st_companion_execution execution;
    memset(&execution, 0, sizeof execution);
    ++flow->executions;
    uint32_t failure = port->execute(port->context, &flow->assessment, &execution);
    if (failure != 0) return finish(flow, st_companion_auth_outcome_for_failure(failure), failure);
    flow->execution = execution;

    bool stops = flow->operation == ST_COMPANION_OPERATION_STOP;
    switch (execution.completion) {
    case ST_COMPANION_COMPLETION_PENDING:
        return finish(flow, ST_COMPANION_AUTH_OUTCOME_PENDING, 0);
    case ST_COMPANION_COMPLETION_RUNNING:
        if (!stops) return finish(flow, ST_COMPANION_AUTH_OUTCOME_COMPLETED_RUNNING, 0);
        break;
    case ST_COMPANION_COMPLETION_STOPPED:
        if (stops) return finish(flow, ST_COMPANION_AUTH_OUTCOME_COMPLETED_STOPPED, 0);
        break;
    default:
        break;
    }
    /* A completion that contradicts the operation is not success. */
    return finish(flow, ST_COMPANION_AUTH_OUTCOME_INDETERMINATE, 0);
}

const char *st_companion_auth_confirmation_title(const st_companion_auth_flow *flow)
{
    switch (flow->operation) {
    case ST_COMPANION_OPERATION_STOP: return "Stop Server?";
    case ST_COMPANION_OPERATION_RESTART: return "Restart Server?";
    case ST_COMPANION_OPERATION_START: break;
    }
    return NULL;
}

const char *st_companion_auth_confirmation_button(const st_companion_auth_flow *flow)
{
    if (!st_companion_operation_is_destructive(flow->operation)) return NULL;
    return st_companion_operation_title(flow->operation);
}

size_t st_companion_auth_confirmation_message(const st_companion_auth_flow *flow,
                                              char *out,
                                              size_t capacity)
{
    if (out == NULL || capacity == 0) return 0;
    out[0] = '\0';
    if (flow->phase != ST_COMPANION_AUTH_CONFIRMING ||
        !st_companion_operation_is_destructive(flow->operation)) {
        return 0;
    }
    char sessions[64];
    char writes[64];
    uint32_t session_count = flow->assessment.terminal_sessions;
    uint32_t write_count = flow->assessment.started_writes;
    if (session_count == 0) {
        snprintf(sessions, sizeof sessions, "No Terminal Sessions will end.");
    } else if (session_count == 1) {
        snprintf(sessions, sizeof sessions, "1 Terminal Session will end.");
    } else {
        snprintf(sessions, sizeof sessions, "%u Terminal Sessions will end.", session_count);
    }
    if (write_count == 0) {
        writes[0] = '\0';
    } else if (write_count == 1) {
        snprintf(writes, sizeof writes, " 1 started Git write finishes first.");
    } else {
        snprintf(writes, sizeof writes, " %u started Git writes finish first.", write_count);
    }
    const char *after = flow->operation == ST_COMPANION_OPERATION_RESTART
                            ? " SpaceTerm Server starts again afterward."
                            : "";
    int written = snprintf(out, capacity,
                           "%s%s%s Next, macOS asks you to authorize this change in a separate "
                           "dialog. Confirming here does not authorize it.",
                           sessions, writes, after);
    if (written <= 0 || (size_t)written >= capacity) {
        out[0] = '\0';
        return 0;
    }
    return (size_t)written;
}
