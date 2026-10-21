/* Keep scanner refusal sticky across native retries on one parsing thread.
 * Scopes are synchronous and exclusive; no status is shared across threads. */
#include "rootlight_gdscript_status.h"

_Static_assert(sizeof(RootlightGDScriptStatus) == sizeof(int), "status ABI requires a C int enum");

#if defined(_MSC_VER)
#define ROOTLIGHT_GDSCRIPT_THREAD_LOCAL __declspec(thread)
#else
#define ROOTLIGHT_GDSCRIPT_THREAD_LOCAL _Thread_local
#endif

static ROOTLIGHT_GDSCRIPT_THREAD_LOCAL uint64_t next_token;
static ROOTLIGHT_GDSCRIPT_THREAD_LOCAL uint64_t active_token;
static ROOTLIGHT_GDSCRIPT_THREAD_LOCAL RootlightGDScriptStatus refusal;

void rootlight_gdscript_scope_refuse(RootlightGDScriptStatus status) {
    if (refusal == ROOTLIGHT_GDSCRIPT_OK) {
        refusal = status;
    }
}

uint64_t rootlight_gdscript_scope_begin(void) {
    if (active_token != 0 || next_token == UINT64_MAX) {
        rootlight_gdscript_scope_refuse(ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
        return 0;
    }
    active_token = ++next_token;
    refusal = ROOTLIGHT_GDSCRIPT_OK;
    return active_token;
}

RootlightGDScriptStatus rootlight_gdscript_scope_status(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_gdscript_scope_refuse(ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
        return ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT;
    }
    return refusal;
}

RootlightGDScriptStatus rootlight_gdscript_scope_finish(uint64_t token) {
    if (token == 0 || active_token != token) {
        rootlight_gdscript_scope_refuse(ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
        return ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT;
    }
    active_token = 0;
    return refusal;
}

bool rootlight_gdscript_scope_ready(void) {
    if (active_token == 0) {
        rootlight_gdscript_scope_refuse(ROOTLIGHT_GDSCRIPT_SCOPE_MISSING);
        return false;
    }
    return refusal == ROOTLIGHT_GDSCRIPT_OK;
}
