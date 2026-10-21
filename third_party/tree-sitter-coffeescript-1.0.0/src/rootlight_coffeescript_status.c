/* Preserve the first refusal outside scanner snapshots and parser rollback.
 * Callback ownership is thread-local and must be held by a thread-affine host
 * guard; numeric tokens are not globally unique capabilities. */
#include "rootlight_coffeescript_status.h"
#include <stddef.h>

_Static_assert(sizeof(RootlightCoffeescriptStatus) == sizeof(int), "status ABI requires C int enum");

#if defined(_MSC_VER)
#define ROOTLIGHT_COFFEESCRIPT_TLS __declspec(thread)
#else
#define ROOTLIGHT_COFFEESCRIPT_TLS _Thread_local
#endif

static ROOTLIGHT_COFFEESCRIPT_TLS uint64_t next_token;
static ROOTLIGHT_COFFEESCRIPT_TLS uint64_t active_token;
static ROOTLIGHT_COFFEESCRIPT_TLS uint64_t work_limit;
static ROOTLIGHT_COFFEESCRIPT_TLS uint64_t work_used;
static ROOTLIGHT_COFFEESCRIPT_TLS RootlightCoffeescriptStatus refusal;
static ROOTLIGHT_COFFEESCRIPT_TLS RootlightCoffeescriptCancelled cancelled;
static ROOTLIGHT_COFFEESCRIPT_TLS void *cancel_context;
static ROOTLIGHT_COFFEESCRIPT_TLS bool callback_active;

void rootlight_coffeescript_scope_refuse(RootlightCoffeescriptStatus status) {
    if (refusal == ROOTLIGHT_COFFEESCRIPT_OK) refusal = status;
}

uint64_t rootlight_coffeescript_scope_begin(uint64_t limit) {
    if (callback_active || active_token != 0 || next_token == UINT64_MAX) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT);
        return 0;
    }
    active_token = ++next_token;
    work_limit = limit;
    work_used = 0;
    refusal = limit ? ROOTLIGHT_COFFEESCRIPT_OK : ROOTLIGHT_COFFEESCRIPT_WORK_LIMIT;
    cancelled = NULL;
    cancel_context = NULL;
    return active_token;
}

bool rootlight_coffeescript_scope_set_cancel(uint64_t token,
    RootlightCoffeescriptCancelled callback, void *context) {
    if (callback_active || token == 0 || active_token != token) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT);
        return false;
    }
    cancelled = callback;
    cancel_context = context;
    return rootlight_coffeescript_scope_poll();
}

bool rootlight_coffeescript_scope_poll(void) {
    if (callback_active) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT);
        return false;
    }
    if (active_token == 0) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_SCOPE_MISSING);
        return false;
    }
    if (refusal == ROOTLIGHT_COFFEESCRIPT_OK && cancelled) {
        callback_active = true;
        bool stop = cancelled(cancel_context);
        callback_active = false;
        if (stop) rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_CANCELLED);
    }
    return refusal == ROOTLIGHT_COFFEESCRIPT_OK;
}

bool rootlight_coffeescript_scope_step(void) {
    if (!rootlight_coffeescript_scope_poll()) return false;
    if (work_used == work_limit) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_WORK_LIMIT);
        return false;
    }
    ++work_used;
    return true;
}

uint64_t rootlight_coffeescript_scope_work(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT);
        return 0;
    }
    return work_used;
}

bool rootlight_coffeescript_scope_context_active(const void *context) {
    return active_token != 0 && cancelled != NULL && cancel_context == context;
}

RootlightCoffeescriptStatus rootlight_coffeescript_scope_status(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT);
        return ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT;
    }
    rootlight_coffeescript_scope_poll();
    return refusal;
}

RootlightCoffeescriptStatus rootlight_coffeescript_scope_finish(uint64_t token) {
    if (callback_active) {
        /* Detach before releasing the C-held Rc, while leaving the operation
         * occupied so reentry cannot begin a fresh successful transaction. */
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT);
        cancelled = NULL;
        cancel_context = NULL;
        return ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT;
    }
    if (token == 0 || active_token != token) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT);
        return ROOTLIGHT_COFFEESCRIPT_SCOPE_CONFLICT;
    }
    rootlight_coffeescript_scope_poll();
    active_token = 0;
    cancelled = NULL;
    cancel_context = NULL;
    return refusal;
}
