/* Anvil runtime: the only code a compiled Anvil program links besides the C runtime.
 * Text formatting here has twins in anvil_front (ir.rs); the parity tests keep them identical.
 *
 * Heap objects (str, arrays and maps; a map is an array of key-value elements sorted by key)
 * are reference counted. The layout is shared with the code
 * generator (anvil_codegen/src/lib.rs): rc at offset 0, kind at 8, length at 16; arrays keep
 * their capacity at 24, element layout at 32 and element storage at 40. Static strings have
 * rc -1 and are never freed. Functions taking objects borrow them; returned objects are owned. */
#define _CRT_SECURE_NO_WARNINGS
#ifndef _WIN32
#define _POSIX_C_SOURCE 200809L /* clock_gettime */
#endif
#ifdef _MSC_VER
#pragma warning(disable : 4200) /* flexible array members are standard C99 */
#endif
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <errno.h>
#ifdef _WIN32
#include <io.h>
#include <fcntl.h>
#include <wchar.h>
__declspec(dllimport) int __stdcall QueryPerformanceCounter(int64_t *count);
__declspec(dllimport) int __stdcall QueryPerformanceFrequency(int64_t *frequency);
__declspec(dllimport) int __stdcall MultiByteToWideChar(unsigned int code_page, unsigned long flags, const char *s,
                                                         int n, wchar_t *w, int wn);
__declspec(dllimport) int __stdcall WideCharToMultiByte(unsigned int code_page, unsigned long flags, const wchar_t *w,
                                                         int wn, char *s, int n, const char *fallback, int *used);
__declspec(dllimport) unsigned long __stdcall GetFileAttributesW(const wchar_t *path);
#else
#include <time.h>
#include <sys/stat.h>
#endif

#define ANVIL_PANIC_EXIT 101
#define ANVIL_RUNTIME_ERROR_EXIT 70

enum { KIND_STR = 0, KIND_ARR = 1 };

typedef struct {
    int64_t rc;
    int64_t kind;
} AnvObj;

typedef struct {
    int64_t rc;
    int64_t kind;
    int64_t len;
    char bytes[];
} AnvStr;

/* Element layout: slot size in bytes, then the byte offsets of the pointers to release. */
typedef struct {
    int64_t elem_size;
    int64_t nptrs;
    int64_t offsets[];
} AnvTypeInfo;

typedef struct {
    int64_t rc;
    int64_t kind;
    int64_t len;
    int64_t cap;
    const AnvTypeInfo *ti;
    char *data;
} AnvArr;

/* Objects allocated and not yet freed; checked at exit when ANVIL_CHECK_LEAKS is set. */
static int64_t live_objects = 0;

/* Output is byte-exact on every platform: no newline translation. */
static void init_stdout(void) {
    static int done = 0;
    if (done) return;
    done = 1;
#ifdef _WIN32
    _setmode(_fileno(stdout), _O_BINARY);
    _setmode(_fileno(stderr), _O_BINARY);
#endif
}

/* A bug in Anvil itself, never in the program: loud, with its own exit code. */
static void runtime_error(const char *what) {
    init_stdout();
    fflush(stdout);
    fprintf(stderr, "anvil runtime error: %s\n", what);
    fflush(stderr);
    exit(ANVIL_RUNTIME_ERROR_EXIT);
}

static void *xmalloc(size_t n) {
    void *p = malloc(n ? n : 1);
    if (!p) runtime_error("out of memory");
    return p;
}

void anvil_retain(AnvObj *o) {
    if (!o || o->rc < 0) return;
    if (o->rc == 0) runtime_error("retain of a freed object");
    o->rc++;
}

void anvil_release(AnvObj *o) {
    if (!o || o->rc < 0) return;
    if (o->rc == 0) runtime_error("release of a freed object");
    if (--o->rc > 0) return;
    if (o->kind == KIND_ARR) {
        AnvArr *a = (AnvArr *)o;
        for (int64_t i = 0; i < a->len; i++) {
            char *elem = a->data + i * a->ti->elem_size;
            for (int64_t k = 0; k < a->ti->nptrs; k++) anvil_release(*(AnvObj **)(elem + a->ti->offsets[k]));
        }
        free(a->data);
    }
    live_objects--;
    free(o);
}

/* ---- strings ---- */

static AnvStr *str_new(int64_t len) {
    AnvStr *s = xmalloc(sizeof(AnvStr) + (size_t)len + 1);
    s->rc = 1;
    s->kind = KIND_STR;
    s->len = len;
    s->bytes[len] = 0;
    live_objects++;
    return s;
}

static AnvStr *str_from(const char *p, int64_t len) {
    AnvStr *s = str_new(len);
    memcpy(s->bytes, p, (size_t)len);
    return s;
}

AnvStr *anvil_str_concat(const AnvStr *a, const AnvStr *b) {
    AnvStr *s = str_new(a->len + b->len);
    memcpy(s->bytes, a->bytes, (size_t)a->len);
    memcpy(s->bytes + a->len, b->bytes, (size_t)b->len);
    return s;
}

/* Bytewise order: -1, 0 or 1. */
int64_t anvil_str_cmp(const AnvStr *a, const AnvStr *b) {
    int64_t n = a->len < b->len ? a->len : b->len;
    int c = memcmp(a->bytes, b->bytes, (size_t)n);
    if (c != 0) return c < 0 ? -1 : 1;
    if (a->len != b->len) return a->len < b->len ? -1 : 1;
    return 0;
}

AnvStr *anvil_str_from_i64(int64_t v) {
    char buf[32];
    int n = snprintf(buf, sizeof buf, "%lld", (long long)v);
    return str_from(buf, n);
}

AnvStr *anvil_str_from_bool(int64_t v) { return v ? str_from("true", 4) : str_from("false", 5); }

/* Shortest digits that round-trip, then Anvil's layout (see docs/SPEC.md, "Text of values").
 * out must hold 64 bytes. Mirrors ir::format_f64. */
static int format_f64(double v, char *out) {
    if (isnan(v)) return snprintf(out, 64, "nan");
    if (isinf(v)) return snprintf(out, 64, v < 0 ? "-inf" : "inf");
    char sci[64];
    int prec;
    for (prec = 1; prec <= 17; prec++) {
        snprintf(sci, sizeof sci, "%.*e", prec - 1, v);
        if (strtod(sci, NULL) == v) break;
    }
    /* sci looks like [-]d[.ddd]e[+-]xx */
    char digits[32];
    int nd = 0, neg = 0;
    const char *p = sci;
    if (*p == '-') { neg = 1; p++; }
    while (*p && *p != 'e') { if (*p != '.') digits[nd++] = *p; p++; }
    int exp10 = atoi(p + 1);
    while (nd > 1 && digits[nd - 1] == '0') nd--;
    int n = 0;
    if (neg) out[n++] = '-';
    if (exp10 >= -5 && exp10 < 17) {
        if (exp10 < 0) {
            out[n++] = '0'; out[n++] = '.';
            for (int i = 0; i < -exp10 - 1; i++) out[n++] = '0';
            for (int i = 0; i < nd; i++) out[n++] = digits[i];
        } else {
            int i;
            for (i = 0; i <= exp10; i++) out[n++] = i < nd ? digits[i] : '0';
            out[n++] = '.';
            if (i >= nd) out[n++] = '0';
            for (; i < nd; i++) out[n++] = digits[i];
        }
    } else {
        out[n++] = digits[0];
        out[n++] = '.';
        if (nd == 1) out[n++] = '0';
        for (int i = 1; i < nd; i++) out[n++] = digits[i];
        n += snprintf(out + n, 64 - n, "e%d", exp10);
    }
    out[n] = 0;
    return n;
}

AnvStr *anvil_str_from_f64(double v) {
    char buf[64];
    int n = format_f64(v, buf);
    return str_from(buf, n);
}

void anvil_print_line(const AnvStr *s) {
    init_stdout();
    fwrite(s->bytes, 1, (size_t)s->len, stdout);
    fputc('\n', stdout);
}

/* print of a number or a bool writes its text straight out: print never allocates. */
void anvil_print_i64(int64_t v) {
    init_stdout();
    fprintf(stdout, "%lld\n", (long long)v);
}

void anvil_print_bool(int64_t v) {
    init_stdout();
    fputs(v ? "true\n" : "false\n", stdout);
}

/* `unit` is the text after the digits: "" or " m/s". */
void anvil_print_f64(double v, const AnvStr *unit) {
    char buf[64];
    int n = format_f64(v, buf);
    init_stdout();
    fwrite(buf, 1, (size_t)n, stdout);
    fwrite(unit->bytes, 1, (size_t)unit->len, stdout);
    fputc('\n', stdout);
}

/* ---- panics and exit ---- */

static void panic_bytes(const char *p, int64_t len) {
    init_stdout();
    fflush(stdout);
    fputs("panic: ", stderr);
    fwrite(p, 1, (size_t)len, stderr);
    fputc('\n', stderr);
    fflush(stderr);
    exit(ANVIL_PANIC_EXIT);
}

void anvil_panic(const AnvStr *message) { panic_bytes(message->bytes, message->len); }

/* Same text as ir::index_message. */
void anvil_panic_index(int64_t index, int64_t len, const AnvStr *site) {
    char buf[512];
    int n = snprintf(buf, sizeof buf, "index %lld out of bounds for length %lld at %.*s", (long long)index,
                     (long long)len, (int)site->len, site->bytes);
    if (n >= (int)sizeof buf) n = (int)sizeof buf - 1;
    panic_bytes(buf, n);
}

/* Called by the generated main after anvil_main returns, so buffered output is never lost. */
void anvil_exit(int64_t code) {
    fflush(stdout);
    if (getenv("ANVIL_CHECK_LEAKS") && live_objects != 0) {
        char buf[64];
        snprintf(buf, sizeof buf, "%lld objects leaked", (long long)live_objects);
        runtime_error(buf);
    }
    exit((int)code);
}

/* main's i64 result becomes the exit code; outside 0..=255 is a panic (same text as ir::main_exit_message). */
void anvil_check_exit(int64_t code) {
    if (code < 0 || code > 255) {
        char buf[128];
        int n = snprintf(buf, sizeof buf, "main returned %lld, outside the exit code range 0..=255", (long long)code);
        panic_bytes(buf, n);
    }
}

/* ---- arrays ---- */

AnvArr *anvil_arr_new(const AnvTypeInfo *ti, int64_t cap) {
    AnvArr *a = xmalloc(sizeof(AnvArr));
    a->rc = 1;
    a->kind = KIND_ARR;
    a->len = 0;
    a->cap = cap;
    a->ti = ti;
    a->data = cap ? xmalloc((size_t)(cap * ti->elem_size)) : NULL;
    live_objects++;
    return a;
}

/* Takes one reference to `a` and returns an array with a single owner: `a` itself if it has
 * no other owners, otherwise a copy (and `a` loses the reference). */
AnvArr *anvil_arr_unique(AnvArr *a) {
    if (a->rc == 1) return a;
    AnvArr *b = anvil_arr_new(a->ti, a->len);
    b->len = a->len;
    if (a->len) memcpy(b->data, a->data, (size_t)(a->len * a->ti->elem_size));
    for (int64_t i = 0; i < b->len; i++) {
        char *elem = b->data + i * b->ti->elem_size;
        for (int64_t k = 0; k < b->ti->nptrs; k++) anvil_retain(*(AnvObj **)(elem + b->ti->offsets[k]));
    }
    a->rc--;
    return b;
}

/* Appends an uninitialised slot to a uniquely owned array and returns its address. */
char *anvil_arr_push_slot(AnvArr *a) {
    if (a->rc != 1) runtime_error("push to a shared array");
    if (a->len == a->cap) {
        if (a->cap > INT64_MAX / 2 / (a->ti->elem_size ? a->ti->elem_size : 1)) runtime_error("array too large");
        int64_t cap = a->cap ? a->cap * 2 : 4;
        char *data = realloc(a->data, (size_t)(cap * a->ti->elem_size) + 1);
        if (!data) runtime_error("out of memory");
        a->data = data;
        a->cap = cap;
    }
    return a->data + a->len++ * a->ti->elem_size;
}

/* Removes the last element of a uniquely owned, non-empty array; its references pass to the caller. */
char *anvil_arr_pop_slot(AnvArr *a) {
    if (a->rc != 1 || a->len == 0) runtime_error("pop from a shared or empty array");
    return a->data + --a->len * a->ti->elem_size;
}

static void retain_elements(AnvArr *a) {
    for (int64_t i = 0; i < a->len; i++) {
        char *elem = a->data + i * a->ti->elem_size;
        for (int64_t k = 0; k < a->ti->nptrs; k++) anvil_retain(*(AnvObj **)(elem + a->ti->offsets[k]));
    }
}

/* Inserts an uninitialised slot at `pos` of a uniquely owned array and returns its address. */
char *anvil_arr_insert_slot(AnvArr *a, int64_t pos) {
    if (pos < 0 || pos > a->len) runtime_error("insert outside an array");
    anvil_arr_push_slot(a);
    int64_t size = a->ti->elem_size;
    char *at = a->data + pos * size;
    memmove(at + size, at, (size_t)((a->len - 1 - pos) * size));
    return at;
}

/* Removes element `pos` of a uniquely owned array; the caller has released its references. */
void anvil_arr_remove_slot(AnvArr *a, int64_t pos) {
    if (a->rc != 1 || pos < 0 || pos >= a->len) runtime_error("remove from a shared array or outside it");
    int64_t size = a->ti->elem_size;
    char *at = a->data + pos * size;
    memmove(at, at + size, (size_t)((a->len - 1 - pos) * size));
    a->len--;
}

/* ---- slices: same text as ir::slice_message and ir::slice_char_message ---- */

static void panic_slice(const char *what, int64_t from, int64_t to, int64_t len, const AnvStr *site) {
    char buf[512];
    int n = len < 0 ? snprintf(buf, sizeof buf, "slice %lld..%lld %s at %.*s", (long long)from, (long long)to, what,
                               (int)site->len, site->bytes)
                    : snprintf(buf, sizeof buf, "slice %lld..%lld %s %lld at %.*s", (long long)from, (long long)to,
                               what, (long long)len, (int)site->len, site->bytes);
    if (n >= (int)sizeof buf) n = (int)sizeof buf - 1;
    panic_bytes(buf, n);
}

AnvArr *anvil_arr_slice(const AnvArr *a, int64_t from, int64_t to, const AnvStr *site) {
    if (from < 0 || to > a->len || from > to) panic_slice("out of bounds for length", from, to, a->len, site);
    AnvArr *b = anvil_arr_new(a->ti, to - from);
    b->len = to - from;
    if (b->len) memcpy(b->data, a->data + from * a->ti->elem_size, (size_t)(b->len * a->ti->elem_size));
    retain_elements(b);
    return b;
}

/* A byte that continues a UTF-8 sequence: slicing there would split a character. */
static int continues_char(const AnvStr *s, int64_t i) {
    return i < s->len && ((unsigned char)s->bytes[i] & 0xC0) == 0x80;
}

AnvStr *anvil_str_slice(const AnvStr *s, int64_t from, int64_t to, const AnvStr *site) {
    if (from < 0 || to > s->len || from > to) panic_slice("out of bounds for length", from, to, s->len, site);
    if (continues_char(s, from) || continues_char(s, to)) panic_slice("splits a character", from, to, -1, site);
    return str_from(s->bytes + from, to - from);
}

/* ---- str search and split ---- */

/* Byte offset of the first occurrence of t in s, or -1; an empty t is found at 0. */
int64_t anvil_str_find(const AnvStr *s, const AnvStr *t) {
    for (int64_t i = 0; i + t->len <= s->len; i++) {
        if (memcmp(s->bytes + i, t->bytes, (size_t)t->len) == 0) return i;
    }
    return -1;
}

int64_t anvil_str_starts(const AnvStr *s, const AnvStr *t) {
    return t->len <= s->len && memcmp(s->bytes, t->bytes, (size_t)t->len) == 0;
}

int64_t anvil_str_ends(const AnvStr *s, const AnvStr *t) {
    return t->len <= s->len && memcmp(s->bytes + s->len - t->len, t->bytes, (size_t)t->len) == 0;
}

/* The pieces between non-overlapping occurrences of sep, left to right (as Rust's str::split). */
AnvArr *anvil_str_split(const AnvStr *s, const AnvStr *sep, const AnvTypeInfo *ti, const AnvStr *message) {
    if (sep->len == 0) anvil_panic(message);
    AnvArr *out = anvil_arr_new(ti, 4);
    int64_t start = 0;
    for (int64_t i = 0; i + sep->len <= s->len;) {
        if (memcmp(s->bytes + i, sep->bytes, (size_t)sep->len) == 0) {
            *(AnvStr **)anvil_arr_push_slot(out) = str_from(s->bytes + start, i - start);
            i += sep->len;
            start = i;
        } else {
            i++;
        }
    }
    *(AnvStr **)anvil_arr_push_slot(out) = str_from(s->bytes + start, s->len - start);
    return out;
}

/* Equality for ==; null (an unused field of an enum variant) equals only null. */
int64_t anvil_str_eq(const AnvStr *a, const AnvStr *b) {
    if (a == b) return 1;
    if (!a || !b) return 0;
    return a->len == b->len && memcmp(a->bytes, b->bytes, (size_t)a->len) == 0;
}

/* ---- maps: an array of (key, value) elements sorted by key, the key in the first slot ---- */

/* Index of `key`, or -(insertion point) - 1. */
int64_t anvil_map_find_i64(const AnvArr *m, int64_t key) {
    int64_t lo = 0, hi = m->len;
    while (lo < hi) {
        int64_t mid = lo + (hi - lo) / 2;
        int64_t k = *(const int64_t *)(m->data + mid * m->ti->elem_size);
        if (k == key) return mid;
        if (k < key) lo = mid + 1;
        else hi = mid;
    }
    return -lo - 1;
}

int64_t anvil_map_find_str(const AnvArr *m, const AnvStr *key) {
    int64_t lo = 0, hi = m->len;
    while (lo < hi) {
        int64_t mid = lo + (hi - lo) / 2;
        int64_t c = anvil_str_cmp(*(const AnvStr **)(m->data + mid * m->ti->elem_size), key);
        if (c == 0) return mid;
        if (c < 0) lo = mid + 1;
        else hi = mid;
    }
    return -lo - 1;
}

/* ---- the clock and the random sequence (docs/SPEC.md, Effects) ---- */

/* With ANVIL_CLOCK=virtual (tests and goldens set it), the n-th read gives n milliseconds, as in
 * the interpreter (ir::VIRTUAL_TICK_NS); otherwise reads count from the start of the program. */
static int clock_virtual = 0;
static int64_t clock_reads = 0;
static int64_t clock_start = 0;

static int64_t monotonic_ns(void) {
#ifdef _WIN32
    int64_t count, frequency;
    QueryPerformanceCounter(&count);
    QueryPerformanceFrequency(&frequency);
    return (int64_t)((double)count * (1e9 / (double)frequency));
#else
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + (int64_t)ts.tv_nsec;
#endif
}

static void init_clock(void) {
    const char *mode = getenv("ANVIL_CLOCK");
    if (mode && strcmp(mode, "virtual") != 0) runtime_error("ANVIL_CLOCK must be unset or \"virtual\"");
    clock_virtual = mode != NULL;
    clock_start = clock_virtual ? 0 : monotonic_ns();
}

int64_t anvil_clock_ns(void) {
    int64_t reads = clock_reads++;
    if (clock_virtual) return reads * 1000000;
    return monotonic_ns() - clock_start;
}

/* SplitMix64 from seed 0; mirrors ir::Random. */
static uint64_t random_state = 0;

static uint64_t random_next(void) {
    random_state += 0x9E3779B97F4A7C15ull;
    uint64_t z = random_state;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
    return z ^ (z >> 31);
}

void anvil_random_seed(int64_t seed) { random_state = (uint64_t)seed; }

/* Same text as ir::random_below_message. */
int64_t anvil_random_below(int64_t n, const AnvStr *site) {
    if (n <= 0) {
        char buf[512];
        int len = snprintf(buf, sizeof buf, "random_below(%lld) needs a bound above 0 at %.*s", (long long)n,
                           (int)site->len, site->bytes);
        if (len >= (int)sizeof buf) len = (int)sizeof buf - 1;
        panic_bytes(buf, len);
    }
    uint64_t m = (uint64_t)n;
    uint64_t reject = (0 - m) % m;
    for (;;) {
        uint64_t x = random_next();
        if (x >= reject) return (int64_t)(x % m);
    }
}

double anvil_random_f64(void) { return (double)(random_next() >> 11) * (1.0 / 9007199254740992.0); }

/* ---- the entry point ---- */

/* Generated: the program's main, or (in a test executable) the test argv[1] names. */
extern void anvil_program(void);

static int saved_argc = 0;
static char **saved_argv = NULL;

/* Test executables leave args() empty: their one argument picks the test. */
static int test_mode = 0;

/* Test executables: `exe N` runs test N. */
int64_t anvil_test_index(void) {
    if (saved_argc != 2) runtime_error("a test executable takes one argument, the index of the test to run");
    char *end;
    long long v = strtoll(saved_argv[1], &end, 10);
    if (*saved_argv[1] == 0 || *end != 0 || v < 0) runtime_error("the test index must be a whole number");
    test_mode = 1;
    return (int64_t)v;
}

void anvil_no_test(int64_t index) {
    char buf[64];
    snprintf(buf, sizeof buf, "no test with index %lld", (long long)index);
    runtime_error(buf);
}

/* ---- input and files (docs/SPEC.md, Input and files) ---- */

/* Whether `s` is UTF-8 as Rust's from_utf8 accepts it: no overlong forms, no surrogates,
 * nothing above U+10FFFF. */
static int utf8_valid(const unsigned char *s, int64_t len) {
    int64_t i = 0;
    while (i < len) {
        unsigned c = s[i];
        if (c < 0x80) {
            i++;
            continue;
        }
        int n;
        unsigned lo = 0x80, hi = 0xBF;
        if (c >= 0xC2 && c <= 0xDF) n = 1;
        else if (c == 0xE0) n = 2, lo = 0xA0;
        else if (c >= 0xE1 && c <= 0xEC) n = 2;
        else if (c == 0xED) n = 2, hi = 0x9F;
        else if (c >= 0xEE && c <= 0xEF) n = 2;
        else if (c == 0xF0) n = 3, lo = 0x90;
        else if (c >= 0xF1 && c <= 0xF3) n = 3;
        else if (c == 0xF4) n = 3, hi = 0x8F;
        else return 0;
        if (i + n >= len) return 0;
        for (int k = 1; k <= n; k++) {
            unsigned b = s[i + k];
            if (b < (k == 1 ? lo : 0x80) || b > (k == 1 ? hi : 0xBF)) return 0;
        }
        i += n + 1;
    }
    return 1;
}

AnvArr *anvil_args(const AnvTypeInfo *ti) {
    int64_t n = test_mode || saved_argc < 1 ? 0 : saved_argc - 1;
    AnvArr *out = anvil_arr_new(ti, n);
    for (int64_t i = 1; i <= n; i++) {
        const char *a = saved_argv[i];
        int64_t len = (int64_t)strlen(a);
        if (!utf8_valid((const unsigned char *)a, len)) {
            char buf[64];
            int m = snprintf(buf, sizeof buf, "argument %lld is not UTF-8 text", (long long)i);
            panic_bytes(buf, m);
        }
        *(AnvStr **)anvil_arr_push_slot(out) = str_from(a, len);
    }
    return out;
}

/* The next line of stdin without "\n" or "\r\n"; NULL at the end. Mirrors the interpreter. */
AnvStr *anvil_read_line(const AnvStr *message) {
    static int ready = 0;
    if (!ready) {
        ready = 1;
#ifdef _WIN32
        _setmode(_fileno(stdin), _O_BINARY);
#endif
    }
    init_stdout();
    fflush(stdout); /* a prompt shows before the program waits */
    size_t cap = 128, len = 0;
    char *buf = xmalloc(cap);
    int c;
    while ((c = getc(stdin)) != EOF) {
        if (len == cap) {
            cap *= 2;
            char *grown = realloc(buf, cap);
            if (!grown) runtime_error("out of memory");
            buf = grown;
        }
        buf[len++] = (char)c;
        if (c == '\n') break;
    }
    if (len == 0) {
        free(buf);
        return NULL;
    }
    if (buf[len - 1] == '\n') {
        len--;
        if (len > 0 && buf[len - 1] == '\r') len--;
    }
    if (!utf8_valid((const unsigned char *)buf, (int64_t)len)) {
        free(buf);
        anvil_panic(message);
    }
    AnvStr *s = str_from(buf, (int64_t)len);
    free(buf);
    return s;
}

/* With ANVIL_FS=memory (tests and goldens set it) writes are kept here for the run and read back
 * by the same path, as in the interpreter; the disk is only read. */
typedef struct {
    char *key;
    char *text;
    int64_t len;
} MemFile;

static int fs_memory = 0;
static MemFile *mem_files = NULL;
static int64_t mem_count = 0, mem_cap = 0;

/* The FileError variant of the last failed read_file or write_file (ir::FILE_ERRORS). */
enum { FILE_NOT_FOUND = 0, FILE_IS_DIRECTORY = 1, FILE_DENIED = 2, FILE_NOT_TEXT = 3, FILE_FAILED = 4 };
static int64_t last_file_error = 0;

int64_t anvil_file_error(void) { return last_file_error; }

static void init_files(void) {
    const char *mode = getenv("ANVIL_FS");
    if (mode && strcmp(mode, "memory") != 0) runtime_error("ANVIL_FS must be unset or \"memory\"");
    fs_memory = mode != NULL;
}

/* A NUL-terminated copy of the path; `\` read as `/` when `key` (ir::memory_file_key). */
static char *path_copy(const AnvStr *p, int key) {
    char *s = xmalloc((size_t)p->len + 1);
    for (int64_t i = 0; i < p->len; i++) s[i] = key && p->bytes[i] == '\\' ? '/' : p->bytes[i];
    s[p->len] = 0;
    return s;
}

static MemFile *mem_find(const char *key, int64_t key_len) {
    for (int64_t i = 0; i < mem_count; i++)
        if ((int64_t)strlen(mem_files[i].key) == key_len && memcmp(mem_files[i].key, key, (size_t)key_len) == 0)
            return &mem_files[i];
    return NULL;
}

#ifdef _WIN32
/* Paths are UTF-8 in Anvil and UTF-16 to Windows. */
static wchar_t *wide(const char *s) {
    int n = MultiByteToWideChar(65001 /* CP_UTF8 */, 0, s, -1, NULL, 0);
    wchar_t *w = xmalloc(sizeof(wchar_t) * (size_t)(n > 0 ? n : 1));
    if (n <= 0 || MultiByteToWideChar(65001, 0, s, -1, w, n) <= 0) w[0] = 0;
    return w;
}
#endif

static int is_directory(const char *path, int64_t len) {
    if ((int64_t)strlen(path) != len) return 0; /* a NUL in the path names nothing */
#ifdef _WIN32
    wchar_t *w = wide(path);
    unsigned long attrs = GetFileAttributesW(w);
    free(w);
    return attrs != 0xFFFFFFFFul && (attrs & 0x10 /* FILE_ATTRIBUTE_DIRECTORY */) != 0;
#else
    struct stat st;
    return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
#endif
}

static FILE *open_file(const char *path, int64_t len, int write) {
    if ((int64_t)strlen(path) != len) {
        errno = EINVAL;
        return NULL;
    }
#ifdef _WIN32
    wchar_t *w = wide(path);
    FILE *f = _wfopen(w, write ? L"wb" : L"rb");
    free(w);
    return f;
#else
    return fopen(path, write ? "wb" : "rb");
#endif
}

static int64_t errno_file_error(void) {
    if (errno == ENOENT) return FILE_NOT_FOUND;
    if (errno == EACCES || errno == EPERM) return FILE_DENIED;
    return FILE_FAILED;
}

/* The whole file as a str; NULL with last_file_error set when it fails. */
AnvStr *anvil_read_file(const AnvStr *path) {
    if (path->len == 0) {
        last_file_error = FILE_NOT_FOUND;
        return NULL;
    }
    if (fs_memory) {
        char *key = path_copy(path, 1);
        MemFile *m = mem_find(key, path->len);
        free(key);
        if (m) return str_from(m->text, m->len);
    }
    char *p = path_copy(path, 0);
    if (is_directory(p, path->len)) {
        free(p);
        last_file_error = FILE_IS_DIRECTORY;
        return NULL;
    }
    FILE *f = open_file(p, path->len, 0);
    free(p);
    if (!f) {
        last_file_error = errno_file_error();
        return NULL;
    }
    size_t cap = 4096, len = 0;
    char *buf = xmalloc(cap);
    for (;;) {
        if (len == cap) {
            cap *= 2;
            char *grown = realloc(buf, cap);
            if (!grown) runtime_error("out of memory");
            buf = grown;
        }
        size_t got = fread(buf + len, 1, cap - len, f);
        len += got;
        if (got == 0) break;
    }
    int failed = ferror(f);
    fclose(f);
    AnvStr *s = NULL;
    if (failed) last_file_error = FILE_FAILED;
    else if (!utf8_valid((const unsigned char *)buf, (int64_t)len)) last_file_error = FILE_NOT_TEXT;
    else s = str_from(buf, (int64_t)len);
    free(buf);
    return s;
}

/* Whether the directory a path goes in exists (a path without one goes in the working directory). */
static int parent_exists(const char *path, int64_t len) {
    int64_t cut = -1;
    for (int64_t i = 0; i < len; i++) {
        if (path[i] == '/') cut = i;
#ifdef _WIN32
        if (path[i] == '\\') cut = i;
#endif
    }
    if (cut < 0) return 1;
    if (cut == 0) return 1; /* the root */
    char *dir = xmalloc((size_t)cut + 1);
    memcpy(dir, path, (size_t)cut);
    dir[cut] = 0;
    int yes = is_directory(dir, cut);
    free(dir);
    return yes;
}

/* Replaces the file's contents: the bytes written, or -1 with last_file_error set. */
int64_t anvil_write_file(const AnvStr *path, const AnvStr *text) {
    if (path->len == 0) {
        last_file_error = FILE_NOT_FOUND;
        return -1;
    }
    char *p = path_copy(path, 0);
    if (is_directory(p, path->len)) {
        free(p);
        last_file_error = FILE_IS_DIRECTORY;
        return -1;
    }
    if (fs_memory) {
        int there = parent_exists(p, path->len);
        free(p);
        if (!there) {
            last_file_error = FILE_NOT_FOUND;
            return -1;
        }
        char *key = path_copy(path, 1);
        char *copy = xmalloc((size_t)text->len + 1);
        memcpy(copy, text->bytes, (size_t)text->len);
        MemFile *m = mem_find(key, path->len);
        if (m) {
            free(key);
            free(m->text);
        } else {
            if (mem_count == mem_cap) {
                mem_cap = mem_cap ? mem_cap * 2 : 8;
                MemFile *grown = realloc(mem_files, sizeof(MemFile) * (size_t)mem_cap);
                if (!grown) runtime_error("out of memory");
                mem_files = grown;
            }
            m = &mem_files[mem_count++];
            m->key = key;
        }
        m->text = copy;
        m->len = text->len;
        return text->len;
    }
    FILE *f = open_file(p, path->len, 1);
    free(p);
    if (!f) {
        last_file_error = errno_file_error();
        return -1;
    }
    size_t put = fwrite(text->bytes, 1, (size_t)text->len, f);
    int closed = fclose(f);
    if (put != (size_t)text->len || closed != 0) {
        last_file_error = FILE_FAILED;
        return -1;
    }
    return text->len;
}

/* ---- trim and parsing (ir::trim_text, parse_i64_text, parse_f64_text) ---- */

static int trimmed(char c) { return c == ' ' || c == '\t' || c == '\r' || c == '\n'; }

AnvStr *anvil_str_trim(const AnvStr *s) {
    int64_t a = 0, b = s->len;
    while (a < b && trimmed(s->bytes[a])) a++;
    while (b > a && trimmed(s->bytes[b - 1])) b--;
    return str_from(s->bytes + a, b - a);
}

/* Whether the last parse succeeded. */
static int64_t parse_ok = 0;

int64_t anvil_parse_ok(void) { return parse_ok; }

int64_t anvil_str_parse_i64(const AnvStr *s) {
    parse_ok = 0;
    int64_t i = 0;
    int neg = i < s->len && s->bytes[i] == '-';
    if (neg) i++;
    if (i == s->len) return 0;
    uint64_t limit = neg ? 9223372036854775808ull : 9223372036854775807ull;
    uint64_t v = 0;
    for (; i < s->len; i++) {
        char c = s->bytes[i];
        if (c < '0' || c > '9') return 0;
        uint64_t d = (uint64_t)(c - '0');
        if (v > (limit - d) / 10) return 0;
        v = v * 10 + d;
    }
    parse_ok = 1;
    return neg ? (int64_t)(0 - v) : (int64_t)v;
}

static int64_t digits_at(const AnvStr *s, int64_t i) {
    int64_t start = i;
    while (i < s->len && s->bytes[i] >= '0' && s->bytes[i] <= '9') i++;
    return i - start;
}

double anvil_str_parse_f64(const AnvStr *s) {
    parse_ok = 0;
    int64_t i = s->len > 0 && s->bytes[0] == '-' ? 1 : 0;
    int64_t n = digits_at(s, i);
    if (n == 0) return 0.0;
    i += n;
    if (i < s->len && s->bytes[i] == '.') {
        n = digits_at(s, i + 1);
        if (n == 0) return 0.0;
        i += 1 + n;
    }
    if (i < s->len && (s->bytes[i] == 'e' || s->bytes[i] == 'E')) {
        i++;
        if (i < s->len && (s->bytes[i] == '+' || s->bytes[i] == '-')) i++;
        n = digits_at(s, i);
        if (n == 0) return 0.0;
        i += n;
    }
    if (i != s->len) return 0.0;
    char *text = path_copy(s, 0);
    double v = strtod(text, NULL);
    free(text);
    if (!isfinite(v)) return 0.0;
    parse_ok = 1;
    return v;
}

#ifdef _WIN32
/* The linker gives the main thread 64 MiB of stack (/STACK). wmain: arguments arrive as UTF-16
 * and are kept as UTF-8. */
int wmain(int argc, wchar_t **wargv) {
    char **argv = xmalloc(sizeof(char *) * ((size_t)argc + 1));
    for (int i = 0; i < argc; i++) {
        int n = WideCharToMultiByte(65001 /* CP_UTF8 */, 0, wargv[i], -1, NULL, 0, NULL, NULL);
        argv[i] = xmalloc((size_t)(n > 0 ? n : 1));
        if (n <= 0 || WideCharToMultiByte(65001, 0, wargv[i], -1, argv[i], n, NULL, NULL) <= 0) argv[i][0] = 0;
    }
    argv[argc] = NULL;
    saved_argc = argc;
    saved_argv = argv;
    init_clock();
    init_files();
    anvil_program();
    return 0;
}
#else
#include <pthread.h>

static void *run_program(void *unused) {
    (void)unused;
    anvil_program();
    return NULL;
}

/* The main thread's stack is the process limit (often 8 MiB), so the program runs on a thread
 * with 64 MiB, as on Windows: deep recursion behaves alike on every platform. */
int main(int argc, char **argv) {
    saved_argc = argc;
    saved_argv = argv;
    init_clock();
    init_files();
    pthread_attr_t attr;
    pthread_t thread;
    if (pthread_attr_init(&attr) != 0 || pthread_attr_setstacksize(&attr, (size_t)64 << 20) != 0 ||
        pthread_create(&thread, &attr, run_program, NULL) != 0)
        runtime_error("cannot start the program's thread");
    pthread_join(thread, NULL);
    return 0;
}
#endif
