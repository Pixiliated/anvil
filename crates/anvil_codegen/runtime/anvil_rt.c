/* Anvil runtime: the only code a compiled Anvil program links besides the C runtime.
 * Text formatting here has twins in anvil_front (ir.rs); the parity tests keep them identical.
 *
 * Heap objects (str and arrays) are reference counted. The layout is shared with the code
 * generator (anvil_codegen/src/lib.rs): rc at offset 0, kind at 8, length at 16; arrays keep
 * their capacity at 24, element layout at 32 and element storage at 40. Static strings have
 * rc -1 and are never freed. Functions taking objects borrow them; returned objects are owned. */
#define _CRT_SECURE_NO_WARNINGS
#ifdef _MSC_VER
#pragma warning(disable : 4200) /* flexible array members are standard C99 */
#endif
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#ifdef _WIN32
#include <io.h>
#include <fcntl.h>
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
