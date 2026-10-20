#if defined(_MSC_VER)
/* The unused GNU attribute does not affect scanner behavior. */
#define __attribute__(attributes)
#endif
#include "../upstream/src/scanner.c"
