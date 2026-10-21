/* Request-local refusal and cancellation for the checked Dockerfile scanner.
 * Native callbacks have no error return channel; a completed tree is admissible
 * only after its exact synchronous scope finishes without a refusal. */
#ifndef ROOTLIGHT_DOCKERFILE_STATUS_H
#define ROOTLIGHT_DOCKERFILE_STATUS_H

#include <stdbool.h>
#include <stdint.h>

typedef enum {
    ROOTLIGHT_DOCKERFILE_OK = 0,
    ROOTLIGHT_DOCKERFILE_ALLOCATION = 1,
    ROOTLIGHT_DOCKERFILE_CAPACITY = 2,
    ROOTLIGHT_DOCKERFILE_MALFORMED_STATE = 3,
    ROOTLIGHT_DOCKERFILE_SCOPE_MISSING = 4,
    ROOTLIGHT_DOCKERFILE_SCOPE_CONFLICT = 5,
    ROOTLIGHT_DOCKERFILE_INVALID_SCALAR = 6,
    ROOTLIGHT_DOCKERFILE_CANCELLED = 7,
    ROOTLIGHT_DOCKERFILE_UNSUPPORTED_MARKER = 8,
    ROOTLIGHT_DOCKERFILE_CALLBACK_PANICKED = 9
} RootlightDockerfileStatus;

typedef bool (*RootlightDockerfileCancelled)(void *context);

/* Begin one synchronous operation on this OS thread. Zero refuses ownership;
 * a nested begin cannot reset or take ownership of an outer operation. */
uint64_t rootlight_dockerfile_scope_begin(void);

/* Install a nonthrowing cancellation callback whose context outlives the scope.
 * The callback runs only synchronously on the owning thread. NULL disables it. */
bool rootlight_dockerfile_scope_set_cancel(uint64_t token,
    RootlightDockerfileCancelled cancelled, void *context);

/* Read or finish the exact active scope. Both poll cancellation, and neither
 * clears a prior refusal. A matching finish outside a callback releases ownership.
 * Reentrant finish detaches callbacks and poisons the still-active operation. */
RootlightDockerfileStatus rootlight_dockerfile_scope_status(uint64_t token);
RootlightDockerfileStatus rootlight_dockerfile_scope_finish(uint64_t token);

/* Scanner callbacks poll before advancing input; first refusal stays sticky
 * through native backtracking, malformed restoration and cancellation. */
bool rootlight_dockerfile_scope_poll(void);
void rootlight_dockerfile_scope_refuse(RootlightDockerfileStatus status);

#endif
