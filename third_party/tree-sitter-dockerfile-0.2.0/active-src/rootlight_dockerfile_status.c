/* Keep the first native scanner refusal within one exclusive parsing thread.
 * A fresh token prevents retries or parser reuse from laundering a refusal. */
#include "rootlight_dockerfile_status.h"
#include <stddef.h>

_Static_assert(sizeof(RootlightDockerfileStatus) == sizeof(int), "status ABI requires a C int enum");

#if defined(_MSC_VER)
#define ROOTLIGHT_DOCKERFILE_TLS __declspec(thread)
#else
#define ROOTLIGHT_DOCKERFILE_TLS _Thread_local
#endif

static ROOTLIGHT_DOCKERFILE_TLS uint64_t next_token;
static ROOTLIGHT_DOCKERFILE_TLS uint64_t active_token;
static ROOTLIGHT_DOCKERFILE_TLS RootlightDockerfileStatus refusal;
static ROOTLIGHT_DOCKERFILE_TLS RootlightDockerfileCancelled cancelled;
static ROOTLIGHT_DOCKERFILE_TLS void *cancel_context;
static ROOTLIGHT_DOCKERFILE_TLS bool callback_active;

void rootlight_dockerfile_scope_refuse(RootlightDockerfileStatus status) {
    if (refusal == ROOTLIGHT_DOCKERFILE_OK) refusal = status;
}

uint64_t rootlight_dockerfile_scope_begin(void) {
    if (callback_active || active_token != 0 || next_token == UINT64_MAX) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT);
        return 0;
    }
    active_token = ++next_token;
    refusal = ROOTLIGHT_DOCKERFILE_OK;
    cancelled = NULL;
    cancel_context = NULL;
    return active_token;
}

bool rootlight_dockerfile_scope_set_cancel(uint64_t token,
    RootlightDockerfileCancelled callback, void *context) {
    if (callback_active || token == 0 || active_token != token) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT);
        return false;
    }
    cancelled = callback;
    cancel_context = context;
    return rootlight_dockerfile_scope_poll();
}

bool rootlight_dockerfile_scope_poll(void) {
    if (callback_active) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT);
        return false;
    }
    if (active_token == 0) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_SCOPE_MISSING);
        return false;
    }
    if (refusal == ROOTLIGHT_DOCKERFILE_OK && cancelled) {
        callback_active = true;
        bool stop = cancelled(cancel_context);
        callback_active = false;
        if (stop) rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_CANCELLED);
    }
    return refusal == ROOTLIGHT_DOCKERFILE_OK;
}

RootlightDockerfileStatus rootlight_dockerfile_scope_status(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT);
        return ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT;
    }
    rootlight_dockerfile_scope_poll();
    return refusal;
}

RootlightDockerfileStatus rootlight_dockerfile_scope_finish(uint64_t token) {
    if (callback_active) {
        /* Keep the operation occupied: a callback cannot close it, begin a new
         * operation and erase its refusal before the native parse returns. */
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT);
        cancelled = NULL;
        cancel_context = NULL;
        return ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT;
    }
    if (token == 0 || active_token != token) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT);
        return ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT;
    }
    rootlight_dockerfile_scope_poll();
    active_token = 0;
    cancelled = NULL;
    cancel_context = NULL;
    return refusal;
}
