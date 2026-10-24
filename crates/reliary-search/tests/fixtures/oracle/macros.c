// Oracle fixture: C definitions incl. macros (B1-B3, A).
#define MAX_SIZE 1024
#define WRAP(fn) wrapped_##fn
#define CLAMP(val, lo, hi) ((val)<(lo)?(lo):((val)>(hi)?(hi):(val)))
#define LOGFMT(fmt, ...) fprintf(stderr, fmt, __VA_ARGS__)
#define HAVE_PROTO
#ifdef HAVE_PROTO
int use_proto(void);
#endif
#if defined(EXTRA) && !defined(LEGACY)
int extra_fn(void);
#endif
#include <stdio.h>
int plain_api(int x) { return x; }
EXPORT int macro_api(int x) { return x; }
static int helper(int x) { return x + 1; }
unsigned long big_count(void) { return 0; }
int Foo_bar_baz(int x) { return x; }
int call_it(void) { return helper(1) + plain_api(2); }
int (*fp_handler)(int x);
bool operator==(const int a, const int b) { return a == b; }
