/* Preserve the first refusal outside scanner snapshots and parser rollback.
 * Callback ownership is thread-local and must be held by a thread-affine host
 * guard; numeric tokens are not globally unique capabilities. */
#include "rootlight_liquid_status.h"
#include <stddef.h>

_Static_assert(sizeof(RootlightLiquidStatus) == sizeof(int), "status ABI requires C int enum");

#if defined(_MSC_VER)
#define ROOTLIGHT_LIQUID_TLS __declspec(thread)
#else
#define ROOTLIGHT_LIQUID_TLS _Thread_local
#endif

static ROOTLIGHT_LIQUID_TLS uint64_t next_token;
static ROOTLIGHT_LIQUID_TLS uint64_t active_token;
static ROOTLIGHT_LIQUID_TLS uint64_t work_limit;
static ROOTLIGHT_LIQUID_TLS uint64_t work_used;
static ROOTLIGHT_LIQUID_TLS RootlightLiquidStatus refusal;
static ROOTLIGHT_LIQUID_TLS RootlightLiquidStatus strict_refusal;
static ROOTLIGHT_LIQUID_TLS RootlightLiquidCancelled cancelled;
static ROOTLIGHT_LIQUID_TLS void *cancel_context;
static ROOTLIGHT_LIQUID_TLS bool callback_active;

void rootlight_liquid_scope_refuse(RootlightLiquidStatus status) {
    if (refusal == ROOTLIGHT_LIQUID_OK) refusal = status;
    if (strict_refusal == ROOTLIGHT_LIQUID_OK && status != ROOTLIGHT_LIQUID_OK
        && status != ROOTLIGHT_LIQUID_CAPACITY && status != ROOTLIGHT_LIQUID_WORK_LIMIT) {
        strict_refusal = status;
    }
}

uint64_t rootlight_liquid_scope_begin(uint64_t limit) {
    if (callback_active || active_token != 0 || next_token == UINT64_MAX) {
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_SCOPE_CONFLICT);
        return 0;
    }
    active_token = ++next_token;
    work_limit = limit;
    work_used = 0;
    refusal = limit ? ROOTLIGHT_LIQUID_OK : ROOTLIGHT_LIQUID_WORK_LIMIT;
    strict_refusal = ROOTLIGHT_LIQUID_OK;
    cancelled = NULL;
    cancel_context = NULL;
    return active_token;
}

static bool poll_cancel(bool final_check);

bool rootlight_liquid_scope_set_cancel(uint64_t token,
    RootlightLiquidCancelled callback, void *context) {
    if (callback_active || token == 0 || active_token != token) {
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_SCOPE_CONFLICT);
        return false;
    }
    cancelled = callback;
    cancel_context = context;
    return poll_cancel(true);
}

static bool poll_cancel(bool final_check) {
    if (callback_active) {
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_SCOPE_CONFLICT);
        return false;
    }
    if (active_token == 0) {
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_SCOPE_MISSING);
        return false;
    }
    if ((final_check || refusal == ROOTLIGHT_LIQUID_OK) && cancelled) {
        callback_active = true;
        bool stop = cancelled(cancel_context);
        callback_active = false;
        if (stop) rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_CANCELLED);
    }
    return refusal == ROOTLIGHT_LIQUID_OK && strict_refusal == ROOTLIGHT_LIQUID_OK;
}

bool rootlight_liquid_scope_poll(void) {
    return poll_cancel(false);
}

bool rootlight_liquid_scope_step(void) {
    if (!rootlight_liquid_scope_poll()) return false;
    if (work_used == work_limit) {
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_WORK_LIMIT);
        return false;
    }
    ++work_used;
    return true;
}

uint64_t rootlight_liquid_scope_work(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_SCOPE_CONFLICT);
        return 0;
    }
    return work_used;
}

bool rootlight_liquid_scope_context_active(const void *context) {
    return active_token != 0 && cancelled != NULL && cancel_context == context;
}

RootlightLiquidStatus rootlight_liquid_scope_status(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_SCOPE_CONFLICT);
        return ROOTLIGHT_LIQUID_SCOPE_CONFLICT;
    }
    rootlight_liquid_scope_poll();
    return strict_refusal != ROOTLIGHT_LIQUID_OK ? strict_refusal : refusal;
}

RootlightLiquidStatus rootlight_liquid_scope_finish(uint64_t token) {
    if (callback_active) {
        /* Detach before releasing the C-held Rc, while leaving the operation
         * occupied so reentry cannot begin a fresh successful transaction. */
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_SCOPE_CONFLICT);
        cancelled = NULL;
        cancel_context = NULL;
        return ROOTLIGHT_LIQUID_SCOPE_CONFLICT;
    }
    if (token == 0 || active_token != token) {
        rootlight_liquid_scope_refuse(ROOTLIGHT_LIQUID_SCOPE_CONFLICT);
        return ROOTLIGHT_LIQUID_SCOPE_CONFLICT;
    }
    /* A soft work/capacity decline cannot suppress final cancellation or
     * lifecycle checks before the host considers exact-source fallback. */
    poll_cancel(true);
    active_token = 0;
    cancelled = NULL;
    cancel_context = NULL;
    return strict_refusal != ROOTLIGHT_LIQUID_OK ? strict_refusal : refusal;
}
