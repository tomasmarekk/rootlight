/* Request-local refusal, cancellation and work credit for the Wolfram scanner.
 * A recovered native tree is admissible only after its exclusive synchronous
 * scope finishes successfully. Tokens identify operations on one OS thread. */
#ifndef ROOTLIGHT_WOLFRAM_STATUS_H
#define ROOTLIGHT_WOLFRAM_STATUS_H

#include <stdbool.h>
#include <stdint.h>

typedef enum {
    ROOTLIGHT_WOLFRAM_OK = 0,
    ROOTLIGHT_WOLFRAM_ALLOCATION = 1,
    ROOTLIGHT_WOLFRAM_CAPACITY = 2,
    ROOTLIGHT_WOLFRAM_MALFORMED_STATE = 3,
    ROOTLIGHT_WOLFRAM_SCOPE_MISSING = 4,
    ROOTLIGHT_WOLFRAM_SCOPE_CONFLICT = 5,
    ROOTLIGHT_WOLFRAM_INVALID_SCALAR = 6,
    ROOTLIGHT_WOLFRAM_CANCELLED = 7,
    ROOTLIGHT_WOLFRAM_WORK_LIMIT = 8,
    ROOTLIGHT_WOLFRAM_UNSUPPORTED_NUL = 9,
    ROOTLIGHT_WOLFRAM_CALLBACK_PANICKED = 11
} RootlightWolframStatus;

typedef bool (*RootlightWolframCancelled)(void *context);

/* Begin one synchronous operation with finite logical scanner work credit.
 * Zero refuses ownership; nested begin cannot reset the outer operation. */
uint64_t rootlight_wolfram_scope_begin(uint64_t work_limit);

/* The caller owns a stable context until matching finish clears the callback.
 * It runs synchronously on this OS thread and must never unwind or throw. */
bool rootlight_wolfram_scope_set_cancel(uint64_t token,
    RootlightWolframCancelled cancelled, void *context);

/* Matching finish polls cancellation even after work/capacity and clears the
 * callback. The first strict refusal outranks an earlier work/capacity decline.
 * Mismatched finish preserves ownership; reentrant finish detaches the callback
 * and keeps the operation occupied. */
RootlightWolframStatus rootlight_wolfram_scope_status(uint64_t token);
RootlightWolframStatus rootlight_wolfram_scope_finish(uint64_t token);

/* After attempted cleanup, this exact pointer check lets an FFI owner retain
 * storage if callback release could not be proven. It grants no scope ownership. */
bool rootlight_wolfram_scope_context_active(const void *context);

/* Poll cancellation without spending credit, or charge one finite logical
 * unit. Scanner entry, scalar advance and state/keyword visits are charged. */
bool rootlight_wolfram_scope_poll(void);
bool rootlight_wolfram_scope_step(void);
void rootlight_wolfram_scope_refuse(RootlightWolframStatus status);
uint64_t rootlight_wolfram_scope_work(uint64_t token);

#endif
