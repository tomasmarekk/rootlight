/* Request-local refusal status for the reviewed GDScript scanner derivative.
 * The native callback ABI has no error channel; callers must check this scope
 * before admitting any parse result, including a clean-looking fallback tree. */
#ifndef ROOTLIGHT_GDSCRIPT_STATUS_H
#define ROOTLIGHT_GDSCRIPT_STATUS_H

#include <stdbool.h>
#include <stdint.h>

typedef enum {
    ROOTLIGHT_GDSCRIPT_OK = 0,
    ROOTLIGHT_GDSCRIPT_ALLOCATION = 1,
    ROOTLIGHT_GDSCRIPT_CAPACITY = 2,
    ROOTLIGHT_GDSCRIPT_MALFORMED_STATE = 3,
    ROOTLIGHT_GDSCRIPT_SCOPE_MISSING = 4,
    ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT = 5
} RootlightGDScriptStatus;

/* Begin one synchronous native operation on this OS thread. A zero token is a
 * refusal; nested operations cannot reset or steal an outer operation's status. */
uint64_t rootlight_gdscript_scope_begin(void);

/* Read sticky status for the exact active token without clearing a refusal. */
RootlightGDScriptStatus rootlight_gdscript_scope_status(uint64_t token);

/* Finish only the matching active scope. The result must be checked before
 * committing facts; cancellation cleanup must also finish the owned scope. */
RootlightGDScriptStatus rootlight_gdscript_scope_finish(uint64_t token);

/* Scanner callbacks use the active scope; they never clear its first refusal. */
bool rootlight_gdscript_scope_ready(void);
void rootlight_gdscript_scope_refuse(RootlightGDScriptStatus status);

#endif
