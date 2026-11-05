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
#include <process.h>
#include <intrin.h>
__declspec(dllimport) unsigned long __stdcall GetActiveProcessorCount(unsigned short group);
__declspec(dllimport) void __stdcall Sleep(unsigned long ms);
__declspec(dllimport) int __stdcall CloseHandle(void *handle);
#define ATOMIC_ADD(p, v) _InterlockedExchangeAdd64((volatile long long *)(p), (v))
#define ATOMIC_CAS(p, expected, desired) _InterlockedCompareExchange64((volatile long long *)(p), (desired), (expected))
#define ATOMIC_SET(p, v) _InterlockedExchange64((volatile long long *)(p), (v))
#define THREAD_LOCAL __declspec(thread)
#else
#include <time.h>
#include <sys/stat.h>
#include <dirent.h>
#include <pthread.h>
#include <unistd.h>
#define ATOMIC_ADD(p, v) __atomic_fetch_add((p), (v), __ATOMIC_SEQ_CST)
static int64_t atomic_cas(int64_t *p, int64_t expected, int64_t desired) {
    __atomic_compare_exchange_n(p, &expected, desired, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
    return expected;
}
#define ATOMIC_CAS(p, expected, desired) atomic_cas((p), (expected), (desired))
#define ATOMIC_SET(p, v) __atomic_store_n((p), (v), __ATOMIC_SEQ_CST)
#define THREAD_LOCAL _Thread_local
#endif

#define ANVIL_PANIC_EXIT 101
#define ANVIL_RUNTIME_ERROR_EXIT 70

/* KIND_TEXT: a str or bytes allocated with room to grow (text_room), which an append to the only
 * reference fills in place. It is a str like any other everywhere else. */
enum { KIND_STR = 0, KIND_ARR = 1, KIND_FN = 2, KIND_TEXT = 3 };

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

/* A function value: the layout of its captured values, its code, then the values. The code
 * takes the object itself first; static objects (refcount -1) capture nothing. */
typedef struct {
    int64_t rc;
    int64_t kind;
    const AnvTypeInfo *ti;
    void *code;
    char env[];
} AnvFn;

/* Objects allocated and not yet freed; checked at exit when ANVIL_CHECK_LEAKS is set. Counted
 * atomically, since parallel_map's workers allocate at once (reference counts are not: workers
 * never share an object). */
static int64_t live_objects = 0;

/* What the program's objects ask of the heap, counted when ANVIL_MEASURE names a file (`anvil
 * measure`), which gets the totals at exit: bytes asked for, the most held at once, and objects
 * made. Runs that are measured use one worker (ANVIL_THREADS=1), so the numbers are the same on
 * every run. */
static int mem_measuring = 0;
static const char *mem_file = NULL;
static int64_t mem_allocated = 0, mem_live = 0, mem_peak = 0, mem_objects = 0;

/* `asked` bytes asked for, changing what is held by `change`. */
static void mem_note(int64_t asked, int64_t change) {
    if (!mem_measuring) return;
    ATOMIC_ADD(&mem_allocated, asked);
    int64_t live = ATOMIC_ADD(&mem_live, change) + change;
    if (live > mem_peak) mem_peak = live;
}

/* A new object of `size` bytes. */
static void mem_object(int64_t size) {
    if (!mem_measuring) return;
    ATOMIC_ADD(&mem_objects, 1);
    mem_note(size, size);
}

static void mem_report(void) {
    FILE *f = fopen(mem_file, "w");
    if (!f) {
        fprintf(stderr, "anvil runtime error: cannot write %s\n", mem_file);
        fflush(stderr);
        _Exit(ANVIL_RUNTIME_ERROR_EXIT);
    }
    fprintf(f, "%lld %lld %lld\n", (long long)mem_allocated, (long long)mem_peak, (long long)mem_objects);
    fclose(f);
}

static void init_measure(void) {
    mem_file = getenv("ANVIL_MEASURE");
    if (mem_file) {
        mem_measuring = 1;
        atexit(mem_report);
    }
}

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

static int64_t text_room(int64_t len);

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
        mem_note(0, -(int64_t)sizeof(AnvArr) - a->cap * a->ti->elem_size);
        free(a->data);
    } else if (o->kind == KIND_FN) {
        AnvFn *f = (AnvFn *)o;
        for (int64_t k = 0; k < f->ti->nptrs; k++) anvil_release(*(AnvObj **)(f->env + f->ti->offsets[k]));
        mem_note(0, -(int64_t)sizeof(AnvFn) - f->ti->elem_size);
    } else {
        const AnvStr *s = (const AnvStr *)o;
        mem_note(0, -(int64_t)sizeof(AnvStr) - (o->kind == KIND_TEXT ? text_room(s->len) : s->len + 1));
    }
    ATOMIC_ADD(&live_objects, -1);
    free(o);
}

/* A closure: the generated code stores the captured values into env. */
AnvFn *anvil_fn_new(const AnvTypeInfo *ti, void *code) {
    AnvFn *f = xmalloc(sizeof(AnvFn) + (size_t)ti->elem_size);
    f->rc = 1;
    f->kind = KIND_FN;
    f->ti = ti;
    f->code = code;
    mem_object((int64_t)sizeof(AnvFn) + ti->elem_size);
    ATOMIC_ADD(&live_objects, 1);
    return f;
}

/* ---- strings ---- */

static AnvStr *str_new(int64_t len) {
    AnvStr *s = xmalloc(sizeof(AnvStr) + (size_t)len + 1);
    s->rc = 1;
    s->kind = KIND_STR;
    s->len = len;
    s->bytes[len] = 0;
    mem_object((int64_t)sizeof(AnvStr) + len + 1);
    ATOMIC_ADD(&live_objects, 1);
    return s;
}

static AnvStr *str_from(const char *p, int64_t len) {
    AnvStr *s = str_new(len);
    memcpy(s->bytes, p, (size_t)len);
    return s;
}

/* The room text of `len` bytes grows into: a power of two above it, at least 16; mirrors
 * ir::text_room. */
static int64_t text_room(int64_t len) {
    int64_t room = 16;
    while (room < len + 1) room *= 2;
    return room;
}

/* `s = s + b` (both backends: ir::append_parts): takes the local's reference to `a` and the
 * reference to `b`. When `a` is the only reference and has room, b is copied onto its end;
 * when it lacks room, it doubles (realloc). Otherwise `a` is shared (or static, or was never
 * grown), so its text is copied into new growable text first: value semantics. */
AnvStr *anvil_str_append(AnvStr *a, AnvStr *b) {
    int64_t len = a->len + b->len;
    AnvStr *s;
    if (a->rc == 1 && a->kind == KIND_TEXT) {
        if (len + 1 > text_room(a->len)) {
            mem_note((int64_t)sizeof(AnvStr) + text_room(len), text_room(len) - text_room(a->len));
            s = realloc(a, sizeof(AnvStr) + (size_t)text_room(len));
        } else {
            s = a;
        }
        if (!s) runtime_error("out of memory");
    } else {
        s = xmalloc(sizeof(AnvStr) + (size_t)text_room(len));
        s->rc = 1;
        s->kind = KIND_TEXT;
        s->len = a->len;
        memcpy(s->bytes, a->bytes, (size_t)a->len);
        mem_object((int64_t)sizeof(AnvStr) + text_room(len));
        ATOMIC_ADD(&live_objects, 1);
        anvil_release((AnvObj *)a);
    }
    memcpy(s->bytes + s->len, b->bytes, (size_t)b->len);
    s->len = len;
    s->bytes[len] = 0;
    anvil_release((AnvObj *)b);
    return s;
}

/* Interpolation: text of exactly `len` bytes, then each part copied in at its offset (taking the
 * part's reference), so an interpolation allocates its result once. */
AnvStr *anvil_str_alloc(int64_t len) {
    return str_new(len);
}

int64_t anvil_str_fill(AnvStr *s, int64_t at, AnvStr *part) {
    int64_t len = part->len;
    memcpy(s->bytes + at, part->bytes, (size_t)len);
    anvil_release((AnvObj *)part);
    return at + len;
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

/* `stream` 0 is stdout (print), 1 stderr (eprint). */
static FILE *stream_of(int64_t stream) {
    init_stdout();
    return stream ? stderr : stdout;
}

void anvil_print_line(const AnvStr *s, int64_t stream) {
    FILE *f = stream_of(stream);
    fwrite(s->bytes, 1, (size_t)s->len, f);
    fputc('\n', f);
}

/* print of a number or a bool writes its text straight out: print never allocates. */
void anvil_print_i64(int64_t v, int64_t stream) { fprintf(stream_of(stream), "%lld\n", (long long)v); }

void anvil_print_bool(int64_t v, int64_t stream) { fputs(v ? "true\n" : "false\n", stream_of(stream)); }

/* `unit` is the text after the digits: "" or " m/s". */
void anvil_print_f64(double v, const AnvStr *unit, int64_t stream) {
    char buf[64];
    int n = format_f64(v, buf);
    FILE *f = stream_of(stream);
    fwrite(buf, 1, (size_t)n, f);
    fwrite(unit->bytes, 1, (size_t)unit->len, f);
    fputc('\n', f);
}

/* ---- the workers of parallel_map ---- */

typedef void (*AnvWorker)(void *f, AnvArr *in, AnvArr *out, int64_t base);

/* One worker's share: its own deep copies of the function and of elements base.., and where a
 * panic it met is kept. */
typedef struct {
    AnvWorker work;
    void *f;
    AnvArr *in;
    AnvArr *out;
    int64_t base;
    int64_t current;     /* the element being worked on */
    int64_t done;        /* 1 once finished or parked */
    int64_t panic_index; /* -1: none */
    char *message;
    int64_t message_len;
    int64_t *first_panic; /* the smallest index that panicked so far */
} Job;

static THREAD_LOCAL Job *current_job = NULL;

/* A panic in a worker: keep it, and stop this thread here without unwinding (the program
 * panics once the other workers are done, with the panic of the smallest index). */
static void worker_panic(const char *p, int64_t len) {
    Job *job = current_job;
    job->message = xmalloc((size_t)len + 1);
    memcpy(job->message, p, (size_t)len);
    job->message_len = len;
    job->panic_index = job->current;
    for (;;) {
        int64_t seen = ATOMIC_ADD(job->first_panic, 0);
        if (job->current >= seen || ATOMIC_CAS(job->first_panic, seen, job->current) == seen) break;
    }
    current_job = NULL;
    ATOMIC_SET(&job->done, 1);
    for (;;) {
#ifdef _WIN32
        Sleep(0xFFFFFFFFul);
#else
        pause();
#endif
    }
}

/* ---- panics and exit ---- */

static void panic_bytes(const char *p, int64_t len) {
    if (current_job) worker_panic(p, len);
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
    mem_object((int64_t)sizeof(AnvArr) + cap * ti->elem_size);
    ATOMIC_ADD(&live_objects, 1);
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
        mem_note(cap * a->ti->elem_size, (cap - a->cap) * a->ti->elem_size);
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

/* The whole file; NULL with last_file_error set when it fails. With `text`, it must be UTF-8. */
static AnvStr *read_any(const AnvStr *path, int text) {
    if (path->len == 0) {
        last_file_error = FILE_NOT_FOUND;
        return NULL;
    }
    if (fs_memory) {
        char *key = path_copy(path, 1);
        MemFile *m = mem_find(key, path->len);
        free(key);
        if (m) {
            if (text && !utf8_valid((const unsigned char *)m->text, m->len)) {
                last_file_error = FILE_NOT_TEXT;
                return NULL;
            }
            return str_from(m->text, m->len);
        }
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
    else if (text && !utf8_valid((const unsigned char *)buf, (int64_t)len)) last_file_error = FILE_NOT_TEXT;
    else s = str_from(buf, (int64_t)len);
    free(buf);
    return s;
}

AnvStr *anvil_read_file(const AnvStr *path) { return read_any(path, 1); }

AnvStr *anvil_read_bytes(const AnvStr *path) { return read_any(path, 0); }

/* ---- bytes (a bytes value is a str object whose bytes need not be UTF-8) ---- */

/* Same text as ir::byte_message. */
AnvStr *anvil_bytes_of(const AnvArr *xs, const AnvStr *site) {
    AnvStr *s = str_new(xs->len);
    for (int64_t i = 0; i < xs->len; i++) {
        int64_t v = *(const int64_t *)(xs->data + i * xs->ti->elem_size);
        if (v < 0 || v > 255) {
            anvil_release((AnvObj *)s);
            char buf[512];
            int len = snprintf(buf, sizeof buf, "bytes_of got %lld, which is not a byte (0..=255), at %.*s",
                               (long long)v, (int)site->len, site->bytes);
            if (len >= (int)sizeof buf) len = (int)sizeof buf - 1;
            panic_bytes(buf, len);
        }
        s->bytes[i] = (char)v;
    }
    return s;
}

/* The same object when it is UTF-8 (a str and bytes share their layout), else NULL. */
AnvStr *anvil_bytes_to_str(AnvStr *b) {
    if (!utf8_valid((const unsigned char *)b->bytes, b->len)) return NULL;
    anvil_retain((AnvObj *)b);
    return b;
}

/* ir::hex_text */
AnvStr *anvil_bytes_hex(const AnvStr *b) {
    static const char digits[] = "0123456789abcdef";
    AnvStr *s = str_new(b->len * 2);
    for (int64_t i = 0; i < b->len; i++) {
        unsigned char c = (unsigned char)b->bytes[i];
        s->bytes[2 * i] = digits[c >> 4];
        s->bytes[2 * i + 1] = digits[c & 15];
    }
    return s;
}

AnvArr *anvil_bytes_to_array(const AnvStr *b, const AnvTypeInfo *ti) {
    AnvArr *out = anvil_arr_new(ti, b->len);
    for (int64_t i = 0; i < b->len; i++) *(int64_t *)anvil_arr_push_slot(out) = (unsigned char)b->bytes[i];
    return out;
}

int64_t anvil_bytes_at(const AnvStr *b, int64_t i, const AnvStr *site) {
    if (i < 0 || i >= b->len) anvil_panic_index(i, b->len, site);
    return (unsigned char)b->bytes[i];
}

AnvStr *anvil_bytes_slice(const AnvStr *b, int64_t from, int64_t to, const AnvStr *site) {
    if (from < 0 || to > b->len || from > to) panic_slice("out of bounds for length", from, to, b->len, site);
    return str_from(b->bytes + from, to - from);
}

/* ---- text and array helpers (ir::case_text, fixed_text; the methods in SPEC, Text) ---- */

AnvStr *anvil_str_replace(const AnvStr *s, const AnvStr *old, const AnvStr *new_, const AnvStr *message) {
    if (old->len == 0) anvil_panic(message);
    int64_t count = 0;
    for (int64_t i = 0; i + old->len <= s->len;) {
        if (memcmp(s->bytes + i, old->bytes, (size_t)old->len) == 0) {
            count++;
            i += old->len;
        } else {
            i++;
        }
    }
    AnvStr *out = str_new(s->len + count * (new_->len - old->len));
    int64_t o = 0;
    for (int64_t i = 0; i < s->len;) {
        if (i + old->len <= s->len && memcmp(s->bytes + i, old->bytes, (size_t)old->len) == 0) {
            memcpy(out->bytes + o, new_->bytes, (size_t)new_->len);
            o += new_->len;
            i += old->len;
        } else {
            out->bytes[o++] = s->bytes[i++];
        }
    }
    return out;
}

AnvStr *anvil_str_case(const AnvStr *s, int64_t upper) {
    AnvStr *out = str_from(s->bytes, s->len);
    for (int64_t i = 0; i < s->len; i++) {
        char c = out->bytes[i];
        if (upper && c >= 'a' && c <= 'z') out->bytes[i] = (char)(c - 32);
        if (!upper && c >= 'A' && c <= 'Z') out->bytes[i] = (char)(c + 32);
    }
    return out;
}

/* Each character (a str is UTF-8, so its first byte gives its length). */
AnvArr *anvil_str_chars(const AnvStr *s, const AnvTypeInfo *ti) {
    AnvArr *out = anvil_arr_new(ti, s->len);
    for (int64_t i = 0; i < s->len;) {
        unsigned char c = (unsigned char)s->bytes[i];
        int64_t n = c < 0x80 ? 1 : c < 0xE0 ? 2 : c < 0xF0 ? 3 : 4;
        *(AnvStr **)anvil_arr_push_slot(out) = str_from(s->bytes + i, n);
        i += n;
    }
    return out;
}

AnvStr *anvil_arr_join(const AnvArr *xs, const AnvStr *sep) {
    int64_t len = 0;
    for (int64_t i = 0; i < xs->len; i++) len += (*(AnvStr **)(xs->data + i * xs->ti->elem_size))->len;
    if (xs->len > 1) len += (xs->len - 1) * sep->len;
    AnvStr *out = str_new(len);
    int64_t o = 0;
    for (int64_t i = 0; i < xs->len; i++) {
        if (i > 0) {
            memcpy(out->bytes + o, sep->bytes, (size_t)sep->len);
            o += sep->len;
        }
        const AnvStr *part = *(AnvStr **)(xs->data + i * xs->ti->elem_size);
        memcpy(out->bytes + o, part->bytes, (size_t)part->len);
        o += part->len;
    }
    return out;
}

AnvStr *anvil_f64_fixed(double v, int64_t n, const AnvStr *message) {
    if (n < 0 || n > 17) anvil_panic(message);
    if (isnan(v)) return str_from("nan", 3);
    if (isinf(v)) return v > 0 ? str_from("inf", 3) : str_from("-inf", 4);
    char small[64];
    int len = snprintf(small, sizeof small, "%.*f", (int)n, v);
    if (len < (int)sizeof small) return str_from(small, len);
    char *big = xmalloc((size_t)len + 1);
    snprintf(big, (size_t)len + 1, "%.*f", (int)n, v);
    AnvStr *s = str_from(big, len);
    free(big);
    return s;
}

/* A copy with each element's references counted again. */
static AnvArr *arr_copy(const AnvArr *a) {
    AnvArr *b = anvil_arr_new(a->ti, a->len);
    b->len = a->len;
    if (b->len) memcpy(b->data, a->data, (size_t)(b->len * a->ti->elem_size));
    retain_elements(b);
    return b;
}

AnvArr *anvil_arr_reversed(const AnvArr *a) {
    AnvArr *b = arr_copy(a);
    int64_t size = a->ti->elem_size;
    char *tmp = xmalloc((size_t)size);
    for (int64_t i = 0, j = b->len - 1; i < j; i++, j--) {
        memcpy(tmp, b->data + i * size, (size_t)size);
        memcpy(b->data + i * size, b->data + j * size, (size_t)size);
        memcpy(b->data + j * size, tmp, (size_t)size);
    }
    free(tmp);
    return b;
}

/* The element kinds `sorted` takes. */
enum { SORT_I64 = 0, SORT_F64 = 1, SORT_STR = 2 };

static int sort_before(const char *x, const char *y, int64_t kind) {
    switch (kind) {
    case SORT_I64: return *(const int64_t *)x < *(const int64_t *)y;
    case SORT_F64: return *(const double *)x < *(const double *)y;
    default: return anvil_str_cmp(*(AnvStr *const *)x, *(AnvStr *const *)y) < 0;
    }
}

/* Stable merge sort of 8-byte elements, as the interpreter's sort is stable. */
static void merge_sort(char *data, char *tmp, int64_t n, int64_t kind) {
    if (n < 2) return;
    int64_t half = n / 2;
    merge_sort(data, tmp, half, kind);
    merge_sort(data + half * 8, tmp, n - half, kind);
    int64_t i = 0, j = half, k = 0;
    while (i < half && j < n) {
        if (sort_before(data + j * 8, data + i * 8, kind)) memcpy(tmp + 8 * k++, data + 8 * j++, 8);
        else memcpy(tmp + 8 * k++, data + 8 * i++, 8);
    }
    while (i < half) memcpy(tmp + 8 * k++, data + 8 * i++, 8);
    while (j < n) memcpy(tmp + 8 * k++, data + 8 * j++, 8);
    memcpy(data, tmp, (size_t)(n * 8));
}

AnvArr *anvil_arr_sorted(const AnvArr *a, int64_t kind, const AnvStr *message) {
    if (kind == SORT_F64)
        for (int64_t i = 0; i < a->len; i++)
            if (isnan(*(const double *)(a->data + i * 8))) anvil_panic(message);
    AnvArr *b = arr_copy(a);
    char *tmp = xmalloc((size_t)(b->len * 8 + 8));
    merge_sort(b->data, tmp, b->len, kind);
    free(tmp);
    return b;
}

/* ---- parallel_map: a map on worker threads, with the result of a map in order ---- */

/* A copy sharing nothing with `o` (static objects are shared: they never change). */
static AnvObj *deep_copy(AnvObj *o) {
    if (!o || o->rc < 0) return o;
    if (o->kind == KIND_STR || o->kind == KIND_TEXT) {
        const AnvStr *s = (const AnvStr *)o;
        return (AnvObj *)str_from(s->bytes, s->len);
    }
    if (o->kind == KIND_ARR) {
        const AnvArr *a = (const AnvArr *)o;
        AnvArr *b = anvil_arr_new(a->ti, a->len);
        b->len = a->len;
        if (b->len) memcpy(b->data, a->data, (size_t)(b->len * a->ti->elem_size));
        for (int64_t i = 0; i < b->len; i++) {
            char *elem = b->data + i * b->ti->elem_size;
            for (int64_t k = 0; k < b->ti->nptrs; k++) {
                AnvObj **slot = (AnvObj **)(elem + b->ti->offsets[k]);
                *slot = deep_copy(*slot);
            }
        }
        return (AnvObj *)b;
    }
    const AnvFn *f = (const AnvFn *)o;
    AnvFn *g = anvil_fn_new(f->ti, f->code);
    memcpy(g->env, f->env, (size_t)f->ti->elem_size);
    for (int64_t k = 0; k < f->ti->nptrs; k++) {
        AnvObj **slot = (AnvObj **)(g->env + f->ti->offsets[k]);
        *slot = deep_copy(*slot);
    }
    return (AnvObj *)g;
}

/* Elements from..to of `a`, deep-copied. */
static AnvArr *slice_copy(const AnvArr *a, int64_t from, int64_t to) {
    AnvArr *part = anvil_arr_new(a->ti, to - from);
    part->len = to - from;
    if (part->len) memcpy(part->data, a->data + from * a->ti->elem_size, (size_t)(part->len * a->ti->elem_size));
    for (int64_t i = 0; i < part->len; i++) {
        char *elem = part->data + i * part->ti->elem_size;
        for (int64_t k = 0; k < part->ti->nptrs; k++) {
            AnvObj **slot = (AnvObj **)(elem + part->ti->offsets[k]);
            *slot = deep_copy(*slot);
        }
    }
    return part;
}

/* Called by the generated worker before element j of its share: 0 when an element before it
 * has panicked, so this one need not run. */
int64_t anvil_parallel_at(int64_t j) {
    Job *job = current_job;
    int64_t i = job->base + j;
    if (i > ATOMIC_ADD(job->first_panic, 0)) return 0;
    ATOMIC_SET(&job->current, i);
    return 1;
}

#ifdef _WIN32
static unsigned __stdcall job_main(void *arg) {
#else
static void *job_main(void *arg) {
#endif
    Job *job = arg;
    current_job = job;
    job->work(job->f, job->in, job->out, job->base);
    current_job = NULL;
    ATOMIC_SET(&job->done, 1);
    return 0;
}

/* Workers to start: ANVIL_THREADS when set (1..=64), else the machine's processors (at most 64). */
static int64_t thread_count(void) {
    const char *given = getenv("ANVIL_THREADS");
    if (given) {
        char *end;
        long long v = strtoll(given, &end, 10);
        if (*given == 0 || *end != 0 || v < 1 || v > 64) runtime_error("ANVIL_THREADS must be a whole number in 1..=64");
        return v;
    }
#ifdef _WIN32
    int64_t n = (int64_t)GetActiveProcessorCount(0xFFFF);
#else
    int64_t n = (int64_t)sysconf(_SC_NPROCESSORS_ONLN);
#endif
    return n < 1 ? 1 : n > 64 ? 64 : n;
}

AnvArr *anvil_parallel_map(AnvArr *in, void *f, const AnvTypeInfo *out_ti, AnvWorker work) {
    int64_t n = in->len;
    AnvArr *out = anvil_arr_new(out_ti, n);
    if (n) memset(out->data, 0, (size_t)(n * out_ti->elem_size));
    out->len = n;
    if (n == 0) return out;
    int64_t workers = thread_count();
    if (workers > n) workers = n;
    Job *jobs = xmalloc(sizeof(Job) * (size_t)workers);
    int64_t first_panic = INT64_MAX;
    /* A worker can itself call parallel_map: it waits here like the main thread does. */
    Job *outer = current_job;
    current_job = NULL;
    for (int64_t k = 0; k < workers; k++) {
        int64_t lo = n * k / workers, hi = n * (k + 1) / workers;
        Job job = {work, deep_copy((AnvObj *)f), slice_copy(in, lo, hi), out, lo, lo, 0, -1, NULL, 0, &first_panic};
        jobs[k] = job;
    }
    for (int64_t k = 0; k < workers; k++) {
#ifdef _WIN32
        uintptr_t h = _beginthreadex(NULL, (unsigned)(64u << 20), job_main, &jobs[k], 0x10000 /* reserve */, NULL);
        if (h == 0) runtime_error("cannot start a worker thread");
        CloseHandle((void *)h);
#else
        pthread_attr_t attr;
        pthread_t thread;
        if (pthread_attr_init(&attr) != 0 || pthread_attr_setstacksize(&attr, (size_t)64 << 20) != 0 ||
            pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED) != 0 ||
            pthread_create(&thread, &attr, job_main, &jobs[k]) != 0)
            runtime_error("cannot start a worker thread");
        pthread_attr_destroy(&attr);
#endif
    }
    /* Wait until every worker is finished or parked, or works past an element that panicked
     * (what it would do no longer counts: a loop would have stopped at that panic). */
    for (;;) {
        int64_t panicked = ATOMIC_ADD(&first_panic, 0), waiting = 0;
        for (int64_t k = 0; k < workers; k++)
            if (!ATOMIC_ADD(&jobs[k].done, 0) && ATOMIC_ADD(&jobs[k].current, 0) <= panicked) waiting++;
        if (waiting == 0) break;
#ifdef _WIN32
        Sleep(1);
#else
        struct timespec ms = {0, 1000000};
        nanosleep(&ms, NULL);
#endif
    }
    current_job = outer;
    const Job *failed = NULL;
    for (int64_t k = 0; k < workers; k++)
        if (jobs[k].panic_index >= 0 && (!failed || jobs[k].panic_index < failed->panic_index)) failed = &jobs[k];
    /* Workers past the panic may still run; exit ends them. */
    if (failed) panic_bytes(failed->message, failed->message_len);
    for (int64_t k = 0; k < workers; k++) {
        anvil_release((AnvObj *)jobs[k].in);
        anvil_release((AnvObj *)jobs[k].f);
    }
    free(jobs);
    return out;
}

/* The amount of a shift, which must be in 0..=63; same text as ir::shift_message. */
int64_t anvil_check_shift(int64_t n, const AnvStr *site) {
    if (n < 0 || n > 63) {
        char buf[512];
        int len = snprintf(buf, sizeof buf, "shift by %lld at %.*s; shifts take 0..=63", (long long)n, (int)site->len,
                           site->bytes);
        if (len >= (int)sizeof buf) len = (int)sizeof buf - 1;
        panic_bytes(buf, len);
    }
    return n;
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
static THREAD_LOCAL int64_t parse_ok = 0;

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

/* ---- the system: exit, env and list_dir (docs/SPEC.md, Input and files) ---- */

/* Same texts as ir::exit_message and ir::exit_in_test_message. No leak check: the program's
 * objects are alive when it stops. */
void anvil_exit_now(int64_t code, const AnvStr *site) {
    char buf[512];
    int len = -1;
    if (test_mode)
        len = snprintf(buf, sizeof buf, "exit(%lld) in a test at %.*s; a test ends by returning", (long long)code,
                       (int)site->len, site->bytes);
    else if (code < 0 || code > 255)
        len = snprintf(buf, sizeof buf, "exit(%lld) is outside the exit code range 0..=255 at %.*s", (long long)code,
                       (int)site->len, site->bytes);
    if (len >= 0) {
        if (len >= (int)sizeof buf) len = (int)sizeof buf - 1;
        panic_bytes(buf, len);
    }
    init_stdout();
    fflush(stdout);
    fflush(stderr);
    exit((int)code);
}

/* The variable's value, NULL when unset, not UTF-8, or named ANVIL_ (ir::RESERVED_ENV_PREFIX). */
AnvStr *anvil_env(const AnvStr *name) {
    if (name->len == 0 || (name->len >= 6 && memcmp(name->bytes, "ANVIL_", 6) == 0)) return NULL;
    char *n = path_copy(name, 0);
    if ((int64_t)strlen(n) != name->len) {
        free(n);
        return NULL;
    }
#ifdef _WIN32
    wchar_t *w = wide(n);
    free(n);
    const wchar_t *v = _wgetenv(w);
    free(w);
    if (!v) return NULL;
    int len = WideCharToMultiByte(65001, 0, v, -1, NULL, 0, NULL, NULL);
    if (len <= 0) return NULL;
    char *u = xmalloc((size_t)len);
    WideCharToMultiByte(65001, 0, v, -1, u, len, NULL, NULL);
    AnvStr *s = str_from(u, len - 1);
    free(u);
    return s;
#else
    const char *v = getenv(n);
    free(n);
    if (!v || !utf8_valid((const unsigned char *)v, (int64_t)strlen(v))) return NULL;
    return str_from(v, (int64_t)strlen(v));
#endif
}

static int exists(const char *path, int64_t len) {
    if ((int64_t)strlen(path) != len) return 0;
#ifdef _WIN32
    wchar_t *w = wide(path);
    unsigned long attrs = GetFileAttributesW(w);
    free(w);
    return attrs != 0xFFFFFFFFul;
#else
    struct stat st;
    return stat(path, &st) == 0;
#endif
}

static int by_bytes(const void *a, const void *b) {
    return (int)anvil_str_cmp(*(AnvStr *const *)a, *(AnvStr *const *)b);
}

/* Appends `name` unless it is there already. */
static void add_name(AnvArr *out, const char *name, int64_t len) {
    for (int64_t i = 0; i < out->len; i++) {
        const AnvStr *s = *(AnvStr **)(out->data + i * 8);
        if (s->len == len && memcmp(s->bytes, name, (size_t)len) == 0) return;
    }
    *(AnvStr **)anvil_arr_push_slot(out) = str_from(name, len);
}

/* The names in a directory, sorted bytewise, NULL with last_file_error set when it fails. With
 * writes in memory, the files this run wrote into it too (as the interpreter's list_dir). */
AnvArr *anvil_list_dir(const AnvStr *path, const AnvTypeInfo *ti) {
    if (path->len == 0) {
        last_file_error = FILE_NOT_FOUND;
        return NULL;
    }
    char *p = path_copy(path, 0);
    if (exists(p, path->len) && !is_directory(p, path->len)) {
        free(p);
        last_file_error = 5; /* NotDirectory */
        return NULL;
    }
    AnvArr *out = anvil_arr_new(ti, 8);
#ifdef _WIN32
    char *pattern = xmalloc((size_t)path->len + 3);
    memcpy(pattern, p, (size_t)path->len);
    memcpy(pattern + path->len, "\\*", 3);
    wchar_t *w = wide(pattern);
    free(pattern);
    struct _wfinddata64_t fd;
    intptr_t h = (int64_t)strlen(p) == path->len ? _wfindfirst64(w, &fd) : -1;
    free(w);
    if (h == -1) {
        last_file_error = errno_file_error();
        free(p);
        anvil_release((AnvObj *)out);
        return NULL;
    }
    do {
        if (wcscmp(fd.name, L".") == 0 || wcscmp(fd.name, L"..") == 0) continue;
        int len = WideCharToMultiByte(65001, 0, fd.name, -1, NULL, 0, NULL, NULL);
        char *u = xmalloc((size_t)(len > 0 ? len : 1));
        if (len <= 0 || WideCharToMultiByte(65001, 0, fd.name, -1, u, len, NULL, NULL) <= 0) len = 1, u[0] = 0;
        add_name(out, u, len - 1);
        free(u);
    } while (_wfindnext64(h, &fd) == 0);
    _findclose(h);
#else
    DIR *d = (int64_t)strlen(p) == path->len ? opendir(p) : NULL;
    if (!d) {
        last_file_error = errno_file_error();
        free(p);
        anvil_release((AnvObj *)out);
        return NULL;
    }
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        int64_t len = (int64_t)strlen(e->d_name);
        if (!utf8_valid((const unsigned char *)e->d_name, len)) {
            closedir(d);
            free(p);
            anvil_release((AnvObj *)out);
            last_file_error = FILE_NOT_TEXT;
            return NULL;
        }
        add_name(out, e->d_name, len);
    }
    closedir(d);
#endif
    free(p);
    if (fs_memory) {
        char *dir = path_copy(path, 1);
        int64_t dl = path->len;
        while (dl > 0 && dir[dl - 1] == '/') dl--;
        int dot = dl == 1 && dir[0] == '.';
        for (int64_t i = 0; i < mem_count; i++) {
            const char *k = mem_files[i].key;
            int64_t kl = (int64_t)strlen(k);
            const char *name = k;
            if (!dot) {
                if (kl <= dl + 1 || memcmp(k, dir, (size_t)dl) != 0 || k[dl] != '/') continue;
                name = k + dl + 1;
            }
            if (*name && !strchr(name, '/')) add_name(out, name, (int64_t)strlen(name));
        }
        free(dir);
    }
    qsort(out->data, (size_t)out->len, 8, by_bytes);
    return out;
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
    init_measure();
    anvil_program();
    return 0;
}
#else
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
    init_measure();
    pthread_attr_t attr;
    pthread_t thread;
    if (pthread_attr_init(&attr) != 0 || pthread_attr_setstacksize(&attr, (size_t)64 << 20) != 0 ||
        pthread_create(&thread, &attr, run_program, NULL) != 0)
        runtime_error("cannot start the program's thread");
    pthread_join(thread, NULL);
    return 0;
}
#endif
