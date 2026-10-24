/* Preserve the first refusal outside scanner snapshots and parser rollback.
 * Callback ownership is thread-local and must be held by a thread-affine host
 * guard; numeric tokens are not globally unique capabilities. */
#include "rootlight_cuda_status.h"
#include <stddef.h>

_Static_assert(sizeof(RootlightCudaStatus) == sizeof(int), "status ABI requires C int enum");

#if defined(_MSC_VER)
#define ROOTLIGHT_CUDA_TLS __declspec(thread)
#else
#define ROOTLIGHT_CUDA_TLS _Thread_local
#endif

static ROOTLIGHT_CUDA_TLS uint64_t next_token;
static ROOTLIGHT_CUDA_TLS uint64_t active_token;
static ROOTLIGHT_CUDA_TLS uint64_t work_limit;
static ROOTLIGHT_CUDA_TLS uint64_t work_used;
static ROOTLIGHT_CUDA_TLS RootlightCudaStatus refusal;
static ROOTLIGHT_CUDA_TLS RootlightCudaStatus strict_refusal;
static ROOTLIGHT_CUDA_TLS RootlightCudaCancelled cancelled;
static ROOTLIGHT_CUDA_TLS void *cancel_context;
static ROOTLIGHT_CUDA_TLS bool callback_active;

void rootlight_cuda_scope_refuse(RootlightCudaStatus status) {
    if (refusal == ROOTLIGHT_CUDA_OK) refusal = status;
    if (strict_refusal == ROOTLIGHT_CUDA_OK && status != ROOTLIGHT_CUDA_OK
        && status != ROOTLIGHT_CUDA_CAPACITY && status != ROOTLIGHT_CUDA_WORK_LIMIT) {
        strict_refusal = status;
    }
}

uint64_t rootlight_cuda_scope_begin(uint64_t limit) {
    if (callback_active || active_token != 0 || next_token == UINT64_MAX) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_SCOPE_CONFLICT);
        return 0;
    }
    active_token = ++next_token;
    work_limit = limit;
    work_used = 0;
    refusal = limit ? ROOTLIGHT_CUDA_OK : ROOTLIGHT_CUDA_WORK_LIMIT;
    strict_refusal = ROOTLIGHT_CUDA_OK;
    cancelled = NULL;
    cancel_context = NULL;
    return active_token;
}

static bool poll_cancel(bool final_check);

bool rootlight_cuda_scope_set_cancel(uint64_t token,
    RootlightCudaCancelled callback, void *context) {
    if (callback_active || token == 0 || active_token != token) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_SCOPE_CONFLICT);
        return false;
    }
    cancelled = callback;
    cancel_context = context;
    return poll_cancel(true);
}

static bool poll_cancel(bool final_check) {
    if (callback_active) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_SCOPE_CONFLICT);
        return false;
    }
    if (active_token == 0) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_SCOPE_MISSING);
        return false;
    }
    if ((final_check || refusal == ROOTLIGHT_CUDA_OK) && cancelled) {
        callback_active = true;
        bool stop = cancelled(cancel_context);
        callback_active = false;
        if (stop) rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_CANCELLED);
    }
    return refusal == ROOTLIGHT_CUDA_OK && strict_refusal == ROOTLIGHT_CUDA_OK;
}

bool rootlight_cuda_scope_poll(void) {
    return poll_cancel(false);
}

bool rootlight_cuda_scope_step(void) {
    if (!rootlight_cuda_scope_poll()) return false;
    if (work_used == work_limit) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_WORK_LIMIT);
        return false;
    }
    ++work_used;
    return true;
}

uint64_t rootlight_cuda_scope_work(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_SCOPE_CONFLICT);
        return 0;
    }
    return work_used;
}

bool rootlight_cuda_scope_context_active(const void *context) {
    return active_token != 0 && cancelled != NULL && cancel_context == context;
}

RootlightCudaStatus rootlight_cuda_scope_status(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_SCOPE_CONFLICT);
        return ROOTLIGHT_CUDA_SCOPE_CONFLICT;
    }
    rootlight_cuda_scope_poll();
    return strict_refusal != ROOTLIGHT_CUDA_OK ? strict_refusal : refusal;
}

RootlightCudaStatus rootlight_cuda_scope_finish(uint64_t token) {
    if (callback_active) {
        /* Detach before releasing the C-held Rc, while leaving the operation
         * occupied so reentry cannot begin a fresh successful transaction. */
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_SCOPE_CONFLICT);
        cancelled = NULL;
        cancel_context = NULL;
        return ROOTLIGHT_CUDA_SCOPE_CONFLICT;
    }
    if (token == 0 || active_token != token) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_SCOPE_CONFLICT);
        return ROOTLIGHT_CUDA_SCOPE_CONFLICT;
    }
    /* A soft work/capacity decline cannot suppress final cancellation or
     * lifecycle checks before the host considers exact-source fallback. */
    poll_cancel(true);
    active_token = 0;
    cancelled = NULL;
    cancel_context = NULL;
    return strict_refusal != ROOTLIGHT_CUDA_OK ? strict_refusal : refusal;
}
