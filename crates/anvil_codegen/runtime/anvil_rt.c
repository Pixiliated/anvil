/* Anvil runtime: the only code a compiled Anvil program links besides the C runtime.
 * Text formatting here has twins in anvil_front (ir.rs); the parity tests keep them identical.
 *
 * Heap objects (str, arrays and maps; a map is an array of key-value elements, see Maps below)
 * are reference counted. The layout is shared with the code
 * generator (anvil_codegen/src/lib.rs): rc at offset 0, kind at 8, length at 16; arrays keep
 * their capacity at 24, element layout at 32 and element storage at 40, which for a small array
 * made at its full size is the object's own end (above its kind's low byte, the kind field keeps
 * how many bytes the object has there). Static strings have
 * rc -1 and are never freed. Functions taking objects borrow them; returned objects are owned. */
#define _CRT_SECURE_NO_WARNINGS
#ifndef _WIN32
#define _POSIX_C_SOURCE 200809L /* clock_gettime */
#define _XOPEN_SOURCE 700       /* sigaltstack, SA_ONSTACK */
#define _DEFAULT_SOURCE         /* fwrite_unlocked */
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
__declspec(dllimport) int __stdcall GetConsoleMode(void *console, unsigned long *mode);
#include <process.h>
#include <intrin.h>
__declspec(dllimport) unsigned long __stdcall GetActiveProcessorCount(unsigned short group);
__declspec(dllimport) void __stdcall Sleep(unsigned long ms);
__declspec(dllimport) int __stdcall CloseHandle(void *handle);
__declspec(dllimport) void *__stdcall AddVectoredExceptionHandler(unsigned long first, long(__stdcall *handler)(void *));
__declspec(dllimport) void *__stdcall OpenThread(unsigned long access, int inherit, unsigned long id);
__declspec(dllimport) unsigned long __stdcall GetCurrentThreadId(void);
__declspec(dllimport) unsigned long __stdcall SuspendThread(void *thread);
__declspec(dllimport) unsigned long __stdcall ResumeThread(void *thread);
__declspec(dllimport) int __stdcall GetThreadContext(void *thread, void *context);
__declspec(dllimport) void *__stdcall CreateWaitableTimerExW(void *attributes, const wchar_t *name, unsigned long flags, unsigned long access);
__declspec(dllimport) int __stdcall SetWaitableTimer(void *timer, const int64_t *due, long period, void *routine, void *arg, int resume);
__declspec(dllimport) unsigned long __stdcall WaitForSingleObject(void *handle, unsigned long ms);
__declspec(dllimport) int __stdcall SetThreadStackGuarantee(unsigned long *size);
#define ATOMIC_ADD(p, v) _InterlockedExchangeAdd64((volatile long long *)(p), (v))
#define ATOMIC_CAS(p, expected, desired) _InterlockedCompareExchange64((volatile long long *)(p), (desired), (expected))
#define ATOMIC_SET(p, v) _InterlockedExchange64((volatile long long *)(p), (v))
#define THREAD_LOCAL __declspec(thread)
#else
#include <time.h>
#include <sys/stat.h>
#include <dirent.h>
#include <pthread.h>
#include <signal.h>
#include <unistd.h>
#include <sys/time.h>
#include <ucontext.h>
#define ATOMIC_ADD(p, v) __atomic_fetch_add((p), (v), __ATOMIC_SEQ_CST)
static int64_t atomic_cas(int64_t *p, int64_t expected, int64_t desired) {
    __atomic_compare_exchange_n(p, &expected, desired, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
    return expected;
}
#define ATOMIC_CAS(p, expected, desired) atomic_cas((p), (expected), (desired))
#define ATOMIC_SET(p, v) __atomic_store_n((p), (v), __ATOMIC_SEQ_CST)
#define THREAD_LOCAL _Thread_local
#endif

extern void anvil_program(void);

#define ANVIL_PANIC_EXIT 101
#define ANVIL_RUNTIME_ERROR_EXIT 70

/* KIND_TEXT: a str or bytes allocated with room to grow (text_room), which an append to the only
 * reference fills in place. It is a str like any other everywhere else. */
enum { KIND_STR = 0, KIND_ARR = 1, KIND_FN = 2, KIND_TEXT = 3, KIND_MAP = 4 };

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

/* One place in a big map's table: the key's hash and its element's position + 1 (0 = empty). */
typedef struct {
    uint32_t hash;
    uint32_t where;
} MapSlot;

/* A map is an array of (key, value) elements, the key in the first slot, and starts out sorted by
 * key. From MAP_SMALL elements on it also has a table finding a key by its hash, and new elements
 * go on the end: only the first `sorted` are in order until anvil_map_settle, which anything that
 * walks the elements calls first. Elements keep their order otherwise, so what a program sees
 * never depends on the table or the hash. */
typedef struct {
    AnvArr a;
    int64_t key_is_str;
    int64_t sorted;
    MapSlot *table;
    int64_t mask;
    uint32_t last_hash; /* of the key the last find on this map looked for */
    int64_t last_key;   /* that key, and where it was: a `get` and then a `set` find one key twice */
    int64_t last_at;
} AnvMap;

/* A function value: the layout of its captured values, its code, then the values. The code
 * takes the object itself first; static objects (refcount -1) capture nothing. */
typedef struct {
    int64_t rc;
    int64_t kind;
    const AnvTypeInfo *ti;
    void *code;
    char env[];
} AnvFn;

/* Objects allocated and not yet freed, counted and checked at exit only when ANVIL_CHECK_LEAKS is
 * set. Counted atomically, since parallel_map's workers allocate at once (reference counts are
 * not: workers never share an object). */
static int leak_checking = 0;
static int64_t live_objects = 0;
/* Elements up to this many bytes can live inside their array's object. */
#define ARR_INSIDE_LARGEST 64
#define KIND_OF(o) ((o)->kind & 255)

#define COUNT_OBJECTS(change)                                  \
    do {                                                       \
        if (leak_checking) ATOMIC_ADD(&live_objects, (change)); \
    } while (0)

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
    leak_checking = getenv("ANVIL_CHECK_LEAKS") != NULL;
    mem_file = getenv("ANVIL_MEASURE");
    if (mem_file) {
        mem_measuring = 1;
        atexit(mem_report);
    }
}

/* Only the main thread prints (a worker's function is pure), so streams need no locks. */
#ifdef _WIN32
#define WRITE_BYTES _fwrite_nolock
#define WRITE_CHAR _fputc_nolock
#else
#define WRITE_BYTES fwrite_unlocked
#define WRITE_CHAR putc_unlocked
#endif

/* Output is byte-exact on every platform: no newline translation. Output that goes to a file or a
 * pipe is written in larger pieces; everything that must show first flushes it. */
static void init_stdout(void) {
    static int done = 0;
    if (done) return;
    done = 1;
#ifdef _WIN32
    _setmode(_fileno(stdout), _O_BINARY);
    _setmode(_fileno(stderr), _O_BINARY);
    /* Not _isatty: it also says yes for NUL, which then pays for a system call on every write. */
    unsigned long console_mode;
    if (!GetConsoleMode((void *)_get_osfhandle(_fileno(stdout)), &console_mode)) setvbuf(stdout, NULL, _IOFBF, 1 << 16);
#else
    if (!isatty(fileno(stdout))) setvbuf(stdout, NULL, _IOFBF, 1 << 16);
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

/* Small objects and small element storage come from per-thread pools: a free list per size, 16
 * bytes apart, refilled from chunks that are never given back, so asking for memory is a few
 * instructions. Whoever frees a block keeps it. A worker's pool goes back to the spares when it
 * finishes, for the next worker. Every block's size is known when it is freed (see the callers).
 * A freed object's rc stays 0, so the checks in anvil_retain and anvil_release still see it. */
#define POOL_STEP 16
#define POOL_LARGEST 1024
#define POOL_CHUNK (256 * 1024)

typedef struct PoolFree {
    int64_t rc;
    struct PoolFree *next;
} PoolFree;

typedef struct Pool {
    char *bump, *end;
    PoolFree *free[POOL_LARGEST / POOL_STEP + 1];
    struct Pool *next_spare;
} Pool;

static THREAD_LOCAL Pool *my_pool = NULL;
static Pool *spare_pools = NULL;
static int64_t spare_lock = 0;

static void lock_spares(void) {
    while (ATOMIC_CAS(&spare_lock, 0, 1) != 0) {
    }
}

static Pool *pool_claim(void) {
    lock_spares();
    Pool *p = spare_pools;
    if (p) spare_pools = p->next_spare;
    ATOMIC_SET(&spare_lock, 0);
    if (!p) {
        p = xmalloc(sizeof(Pool));
        memset(p, 0, sizeof(Pool));
    }
    my_pool = p;
    return p;
}

static void pool_give_back(void) {
    Pool *p = my_pool;
    if (!p) return;
    my_pool = NULL;
    lock_spares();
    p->next_spare = spare_pools;
    spare_pools = p;
    ATOMIC_SET(&spare_lock, 0);
}

static size_t pool_class(size_t n) { return n ? (n + POOL_STEP - 1) / POOL_STEP : 1; }

static void *pool_alloc(size_t n) {
    if (n > POOL_LARGEST) return xmalloc(n);
    size_t c = pool_class(n);
    Pool *p = my_pool ? my_pool : pool_claim();
    PoolFree *f = p->free[c];
    if (f) {
        p->free[c] = f->next;
        return f;
    }
    size_t size = c * POOL_STEP;
    if ((size_t)(p->end - p->bump) < size) {
        p->bump = xmalloc(POOL_CHUNK);
        p->end = p->bump + POOL_CHUNK;
    }
    void *block = p->bump;
    p->bump += size;
    return block;
}

static void pool_free(void *block, size_t n) {
    if (n > POOL_LARGEST) {
        free(block);
        return;
    }
    size_t c = pool_class(n);
    Pool *p = my_pool ? my_pool : pool_claim();
    PoolFree *f = block;
    f->rc = 0;
    f->next = p->free[c];
    p->free[c] = f;
}

/* `block` (may be NULL when old is 0) resized from `old` to `new_size` bytes. */
static void *pool_resize(void *block, size_t old, size_t new_size) {
    if (!block) return pool_alloc(new_size);
    if (old > POOL_LARGEST && new_size > POOL_LARGEST) {
        void *grown = realloc(block, new_size);
        if (!grown) runtime_error("out of memory");
        return grown;
    }
    if (old <= POOL_LARGEST && new_size <= POOL_LARGEST && pool_class(old) == pool_class(new_size)) return block;
    void *grown = pool_alloc(new_size);
    memcpy(grown, block, old < new_size ? old : new_size);
    pool_free(block, old);
    return grown;
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
    COUNT_OBJECTS(-1);
    if (KIND_OF(o) == KIND_ARR || KIND_OF(o) == KIND_MAP) {
        AnvArr *a = (AnvArr *)o;
        int64_t size = a->ti->elem_size, nptrs = a->ti->nptrs;
        if (nptrs) {
            for (int64_t i = 0; i < a->len; i++) {
                char *elem = a->data + i * size;
                for (int64_t k = 0; k < nptrs; k++) anvil_release(*(AnvObj **)(elem + a->ti->offsets[k]));
            }
        }
        int64_t header = KIND_OF(o) == KIND_MAP ? (int64_t)sizeof(AnvMap) : (int64_t)sizeof(AnvArr);
        mem_note(0, -header - a->cap * size);
        if (a->data && a->data != (char *)(a + 1)) pool_free(a->data, (size_t)(a->cap * size));
        if (KIND_OF(o) == KIND_MAP && ((AnvMap *)o)->table) {
            AnvMap *m = (AnvMap *)o;
            mem_note(0, -(m->mask + 1) * (int64_t)sizeof(MapSlot));
            free(m->table);
        }
        pool_free(o, (size_t)header + (size_t)(o->kind >> 8));
    } else if (o->kind == KIND_FN) {
        AnvFn *f = (AnvFn *)o;
        for (int64_t k = 0; k < f->ti->nptrs; k++) anvil_release(*(AnvObj **)(f->env + f->ti->offsets[k]));
        mem_note(0, -(int64_t)sizeof(AnvFn) - f->ti->elem_size);
        pool_free(o, sizeof(AnvFn) + (size_t)f->ti->elem_size);
    } else {
        const AnvStr *s = (const AnvStr *)o;
        int64_t size = (int64_t)sizeof(AnvStr) + (o->kind == KIND_TEXT ? text_room(s->len) : s->len + 1);
        mem_note(0, -size);
        pool_free(o, (size_t)size);
    }
}

/* A closure: the generated code stores the captured values into env. */
AnvFn *anvil_fn_new(const AnvTypeInfo *ti, void *code) {
    AnvFn *f = pool_alloc(sizeof(AnvFn) + (size_t)ti->elem_size);
    f->rc = 1;
    f->kind = KIND_FN;
    f->ti = ti;
    f->code = code;
    mem_object((int64_t)sizeof(AnvFn) + ti->elem_size);
    COUNT_OBJECTS(1);
    return f;
}

/* ---- strings ---- */

static AnvStr *str_new(int64_t len) {
    AnvStr *s = pool_alloc(sizeof(AnvStr) + (size_t)len + 1);
    s->rc = 1;
    s->kind = KIND_STR;
    s->len = len;
    s->bytes[len] = 0;
    mem_object((int64_t)sizeof(AnvStr) + len + 1);
    COUNT_OBJECTS(1);
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
            s = pool_resize(a, sizeof(AnvStr) + (size_t)text_room(a->len), sizeof(AnvStr) + (size_t)text_room(len));
        } else {
            s = a;
        }
    } else {
        s = pool_alloc(sizeof(AnvStr) + (size_t)text_room(len));
        s->rc = 1;
        s->kind = KIND_TEXT;
        s->len = a->len;
        memcpy(s->bytes, a->bytes, (size_t)a->len);
        mem_object((int64_t)sizeof(AnvStr) + text_room(len));
        COUNT_OBJECTS(1);
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

/* The digits of `v` end at `end`; returns where they start. */
static char *i64_digits(int64_t v, char *end) {
    uint64_t u = v < 0 ? 0 - (uint64_t)v : (uint64_t)v;
    do {
        *--end = (char)('0' + u % 10);
        u /= 10;
    } while (u);
    if (v < 0) *--end = '-';
    return end;
}

AnvStr *anvil_str_from_i64(int64_t v) {
    char buf[32];
    char *start = i64_digits(v, buf + sizeof buf);
    return str_from(start, buf + sizeof buf - start);
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

void anvil_print_raw(const AnvStr *s) {
    init_stdout();
    WRITE_BYTES(s->bytes, 1, (size_t)s->len, stdout);
}

void anvil_print_line(const AnvStr *s, int64_t stream) {
    FILE *f = stream_of(stream);
    WRITE_BYTES(s->bytes, 1, (size_t)s->len, f);
    WRITE_CHAR('\n', f);
}

/* print of a number or a bool writes its text straight out: print never allocates. */
void anvil_print_i64(int64_t v, int64_t stream) {
    char buf[32];
    buf[31] = '\n';
    char *start = i64_digits(v, buf + 31);
    WRITE_BYTES(start, 1, (size_t)(buf + sizeof buf - start), stream_of(stream));
}

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

/* Waits for the thread that is ending the program to end it. */
static void park(void) {
    for (;;) {
#ifdef _WIN32
        Sleep(0xFFFFFFFFul);
#else
        pause();
#endif
    }
}

/* Workers can run out of stack in the same moment: only the first says so. */
static void end_alone(void) {
    static int64_t ending = 0;
    static THREAD_LOCAL int mine = 0;
    if (mine) _Exit(ANVIL_PANIC_EXIT); /* failed while ending: say nothing more */
    mine = 1;
    if (ATOMIC_CAS(&ending, 0, 1) != 0) park();
}

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
    park();
}

/* ---- panics and exit ---- */

static void panic_bytes(const char *p, int64_t len) {
    if (current_job) worker_panic(p, len);
    end_alone();
    init_stdout();
    fflush(stdout);
    fputs("panic: ", stderr);
    fwrite(p, 1, (size_t)len, stderr);
    fputc('\n', stderr);
    fflush(stderr);
    exit(ANVIL_PANIC_EXIT);
}

void anvil_panic(const AnvStr *message) { panic_bytes(message->bytes, message->len); }

/* panic(message): the message and where, as the interpreter words it. */
void anvil_panic_at(const AnvStr *message, const AnvStr *site) {
    size_t n = (size_t)message->len + (size_t)site->len + 3;
    char *buf = xmalloc(n);
    memcpy(buf, message->bytes, (size_t)message->len);
    memcpy(buf + message->len, " (", 2);
    memcpy(buf + message->len + 2, site->bytes, (size_t)site->len);
    buf[n - 1] = ')';
    panic_bytes(buf, (int64_t)n);
}

/* What a failed assert(a == b) says, worded as ir::failed_sides words it. */
static char *put_bytes(char *to, const char *from, size_t n) {
    memcpy(to, from, n);
    return to + n;
}

static char *put_text(char *to, const char *s) { return put_bytes(to, s, strlen(s)); }

static char *put_quoted(char *to, const char *s, int64_t n) {
    *to++ = '"';
    for (int64_t i = 0; i < n; i++) {
        switch (s[i]) {
        case '"': to = put_text(to, "\\\""); break;
        case '\\': to = put_text(to, "\\\\"); break;
        case '\n': to = put_text(to, "\\n"); break;
        case '\r': to = put_text(to, "\\r"); break;
        case '\t': to = put_text(to, "\\t"); break;
        default: *to++ = s[i];
        }
    }
    *to++ = '"';
    return to;
}

static char *put_part(char *to, const AnvStr *s, int64_t same) {
    int64_t from = same > 40 ? same - 40 : 0;
    if (from > s->len) from = s->len;
    while (from > 0 && from < s->len && ((unsigned char)s->bytes[from] & 0xC0) == 0x80) from--;
    int64_t end = from + 200 < s->len ? from + 200 : s->len;
    while (end > from && end < s->len && ((unsigned char)s->bytes[end] & 0xC0) == 0x80) end--;
    if (from > 0) to = put_text(to, "...");
    to = put_quoted(to, s->bytes + from, end - from);
    if (end < s->len) to = put_text(to, "...");
    return to;
}

void anvil_panic_sides(const AnvStr *message, const AnvStr *left, const AnvStr *right, int64_t text) {
    size_t room = (size_t)message->len + 2 * (size_t)left->len + 2 * (size_t)right->len + 200;
    char *buf = xmalloc(room);
    char *to = put_bytes(buf, message->bytes, (size_t)message->len);
    to = put_text(to, "\n  left:  ");
    if (!text) {
        to = put_bytes(to, left->bytes, (size_t)left->len);
        to = put_text(to, "\n  right: ");
        to = put_bytes(to, right->bytes, (size_t)right->len);
    } else if (left->len <= 300 && right->len <= 300) {
        to = put_quoted(to, left->bytes, left->len);
        to = put_text(to, "\n  right: ");
        to = put_quoted(to, right->bytes, right->len);
    } else {
        int64_t same = 0;
        while (same < left->len && same < right->len && left->bytes[same] == right->bytes[same]) same++;
        to = put_part(to, left, same);
        to = put_text(to, "\n  right: ");
        to = put_part(to, right, same);
        char tail[96];
        int n = snprintf(tail, sizeof tail, "\n  first difference at byte %lld, of %lld and %lld bytes", (long long)same,
                         (long long)left->len, (long long)right->len);
        to = put_bytes(to, tail, (size_t)n);
    }
    panic_bytes(buf, (int64_t)(to - buf));
}

/* ---- running out of stack: a panic like any other, where the system would end the program
 * with no word (the text is the interpreter's). It runs on what is kept for it: a stack of its
 * own on POSIX, the room SetThreadStackGuarantee keeps on Windows. */

static void stack_overflow(void) {
    static const char text[] = "panic: stack overflow: calls nested too deeply\n";
    end_alone();
    init_stdout();
    fflush(stdout);
    fwrite(text, 1, sizeof text - 1, stderr);
    fflush(stderr);
    if (mem_measuring) mem_report();
    _Exit(ANVIL_PANIC_EXIT);
}

#define THREAD_STACK ((size_t)64 << 20)
#define STACK_RESERVE ((size_t)64 << 10)

#ifdef _WIN32
static long __stdcall on_exception(void *info) {
    const unsigned long code = **(const unsigned long **)info; /* EXCEPTION_POINTERS, EXCEPTION_RECORD */
    if (code == 0xC00000FDul) stack_overflow();
    return 0; /* look further */
}

static void guard_stack(void) {
    unsigned long reserve = (unsigned long)STACK_RESERVE;
    SetThreadStackGuarantee(&reserve);
}

static void init_stack_guard(void) { AddVectoredExceptionHandler(1, on_exception); }
#else
static THREAD_LOCAL uintptr_t stack_end = 0;

static void on_fault(int sig, siginfo_t *info, void *context) {
    (void)sig;
    (void)context;
    const uintptr_t at = (uintptr_t)info->si_addr;
    /* In or just past the page at the far end of this thread's stack. */
    if (stack_end && at >= stack_end - ((uintptr_t)1 << 20) && at < stack_end + ((uintptr_t)1 << 18)) stack_overflow();
    signal(SIGSEGV, SIG_DFL); /* anything else is what it always was */
}

static void guard_stack(void) {
    char here;
    stack_end = (uintptr_t)&here - THREAD_STACK;
    stack_t alt;
    alt.ss_sp = xmalloc(STACK_RESERVE);
    alt.ss_size = STACK_RESERVE;
    alt.ss_flags = 0;
    if (sigaltstack(&alt, NULL) != 0) runtime_error("cannot set up the stack guard");
}

static void init_stack_guard(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = on_fault;
    action.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) runtime_error("cannot set up the stack guard");
}
#endif

/* Same text as ir::index_message. */
void anvil_panic_index(int64_t index, int64_t len, const AnvStr *site) {
    char buf[512];
    int n = snprintf(buf, sizeof buf, "index %lld out of bounds for length %lld at %.*s", (long long)index,
                     (long long)len, (int)site->len, site->bytes);
    if (n >= (int)sizeof buf) n = (int)sizeof buf - 1;
    panic_bytes(buf, n);
}

/* Called by the generated main after anvil_main returns, so buffered output is never lost. */
/* ---- sampling: `anvil hot` sets ANVIL_SAMPLE to a file, which gets the address `anvil_program` has
 * and then where the program was about every millisecond, in hexadecimal, a line each. On Windows a
 * thread stops the main thread to look (it must not allocate: the main thread may hold the heap's
 * lock); elsewhere a profiling timer interrupts whichever thread is running. ---- */

#define SAMPLES_MAX (1 << 21)
static uint64_t *samples = NULL;
static volatile int64_t sample_count = 0;
static const char *sample_file = NULL;

static void write_samples(void) {
    FILE *f = fopen(sample_file, "w");
    if (!f) {
        fprintf(stderr, "anvil runtime error: cannot write %s\n", sample_file);
        fflush(stderr);
        _Exit(ANVIL_RUNTIME_ERROR_EXIT);
    }
    fprintf(f, "%llx\n", (unsigned long long)(uintptr_t)&anvil_program);
    for (int64_t i = 0; i < sample_count; i++) fprintf(f, "%llx\n", (unsigned long long)samples[i]);
    fclose(f);
}

#ifdef _WIN32
static volatile int64_t sampling_stops = 0, sampling_stopped = 0;

static unsigned __stdcall sample_main(void *main_thread) {
    /* An x64 CONTEXT: its flags at 0x30, the instruction pointer at 0xF8. */
    __declspec(align(16)) static char context[1232];
    void *timer = CreateWaitableTimerExW(NULL, NULL, 2 /* high resolution */, 0x1F0003);
    while (!ATOMIC_ADD(&sampling_stops, 0)) {
        int64_t due = -10000;
        if (timer && SetWaitableTimer(timer, &due, 0, NULL, NULL, 0)) WaitForSingleObject(timer, 100);
        else Sleep(1);
        if (SuspendThread(main_thread) == 0xFFFFFFFFul) break;
        *(unsigned long *)(context + 0x30) = 0x100001; /* CONTEXT_CONTROL */
        int got = GetThreadContext(main_thread, context);
        ResumeThread(main_thread);
        if (got && sample_count < SAMPLES_MAX) samples[sample_count++] = *(uint64_t *)(context + 0xF8);
    }
    ATOMIC_SET(&sampling_stopped, 1);
    return 0;
}

static void finish_sampling(void) {
    ATOMIC_SET(&sampling_stops, 1);
    while (!ATOMIC_ADD(&sampling_stopped, 0)) Sleep(1);
    write_samples();
}

static void init_sampling(void) {
    sample_file = getenv("ANVIL_SAMPLE");
    if (!sample_file) return;
    samples = xmalloc(sizeof(uint64_t) * SAMPLES_MAX);
    void *me = OpenThread(0x1FFFFF, 0, GetCurrentThreadId());
    if (!me || !_beginthreadex(NULL, 0, sample_main, me, 0, NULL)) runtime_error("cannot start sampling");
    atexit(finish_sampling);
}
#elif defined(__linux__) && (defined(__x86_64__) || defined(__aarch64__))
static void on_sample(int sig, siginfo_t *info, void *context) {
    (void)sig;
    (void)info;
    ucontext_t *u = context;
    int64_t at = sample_count;
    if (at >= SAMPLES_MAX) return;
    sample_count = at + 1;
#ifdef __x86_64__
    samples[at] = (uint64_t)u->uc_mcontext.gregs[16]; /* REG_RIP */
#else
    samples[at] = (uint64_t)u->uc_mcontext.pc;
#endif
}

static void finish_sampling(void) {
    struct itimerval off = {{0, 0}, {0, 0}};
    setitimer(ITIMER_PROF, &off, NULL);
    write_samples();
}

static void init_sampling(void) {
    sample_file = getenv("ANVIL_SAMPLE");
    if (!sample_file) return;
    samples = xmalloc(sizeof(uint64_t) * SAMPLES_MAX);
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = on_sample;
    action.sa_flags = SA_SIGINFO | SA_RESTART;
    sigemptyset(&action.sa_mask);
    struct itimerval every = {{0, 1000}, {0, 1000}};
    if (sigaction(SIGPROF, &action, NULL) != 0 || setitimer(ITIMER_PROF, &every, NULL) != 0) runtime_error("cannot start sampling");
    atexit(finish_sampling);
}
#else
static void init_sampling(void) {
    sample_file = getenv("ANVIL_SAMPLE");
    if (sample_file) runtime_error("sampling is not available on this platform");
}
#endif

void anvil_exit(int64_t code) {
    fflush(stdout);
    if (leak_checking && live_objects != 0) {
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

static void arr_init(AnvArr *a, const AnvTypeInfo *ti, int64_t cap, int64_t kind, int64_t header) {
    a->rc = 1;
    a->kind = kind;
    a->len = 0;
    a->cap = cap;
    a->ti = ti;
    a->data = cap ? pool_alloc((size_t)(cap * ti->elem_size)) : NULL;
    mem_object(header + cap * ti->elem_size);
    COUNT_OBJECTS(1);
}

AnvArr *anvil_arr_new(const AnvTypeInfo *ti, int64_t cap) {
    int64_t bytes = cap * ti->elem_size;
    if (cap > 0 && bytes <= ARR_INSIDE_LARGEST) {
        AnvArr *a = pool_alloc(sizeof(AnvArr) + (size_t)bytes);
        a->rc = 1;
        a->kind = KIND_ARR | (bytes << 8);
        a->len = 0;
        a->cap = cap;
        a->ti = ti;
        a->data = (char *)(a + 1);
        mem_object((int64_t)sizeof(AnvArr) + bytes);
        COUNT_OBJECTS(1);
        return a;
    }
    AnvArr *a = pool_alloc(sizeof(AnvArr));
    arr_init(a, ti, cap, KIND_ARR, (int64_t)sizeof(AnvArr));
    return a;
}

static AnvMap *map_clone(const AnvMap *m);

static void retain_elements(AnvArr *a) {
    /* Numbers have nothing to retain, and walking them would cost more than copying them did. */
    if (!a->ti->nptrs) return;
    for (int64_t i = 0; i < a->len; i++) {
        char *elem = a->data + i * a->ti->elem_size;
        for (int64_t k = 0; k < a->ti->nptrs; k++) anvil_retain(*(AnvObj **)(elem + a->ti->offsets[k]));
    }
}

/* Takes one reference to `a` and returns an array with a single owner: `a` itself if it has
 * no other owners, otherwise a copy (and `a` loses the reference). */
AnvArr *anvil_arr_unique(AnvArr *a) {
    if (a->rc == 1) return a;
    AnvArr *b = KIND_OF(a) == KIND_MAP ? &map_clone((const AnvMap *)a)->a : anvil_arr_new(a->ti, a->len);
    b->len = a->len;
    if (a->len) memcpy(b->data, a->data, (size_t)(a->len * a->ti->elem_size));
    retain_elements(b);
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
        if (a->data == (char *)(a + 1)) {
            /* The object keeps its size; the elements move out of it for good. */
            char *data = pool_alloc((size_t)(cap * a->ti->elem_size));
            memcpy(data, a->data, (size_t)(a->len * a->ti->elem_size));
            a->data = data;
        } else {
            a->data = pool_resize(a->data, (size_t)(a->cap * a->ti->elem_size), (size_t)(cap * a->ti->elem_size));
        }
        a->cap = cap;
    }
    return a->data + a->len++ * a->ti->elem_size;
}


/* Inserts an uninitialised slot at `pos` of a uniquely owned array and returns its address. */
static char *arr_insert_slot(AnvArr *a, int64_t pos) {
    if (pos < 0 || pos > a->len) runtime_error("insert outside an array");
    anvil_arr_push_slot(a);
    int64_t size = a->ti->elem_size;
    char *at = a->data + pos * size;
    memmove(at + size, at, (size_t)((a->len - 1 - pos) * size));
    return at;
}

/* Removes element `pos` of a uniquely owned array; the caller has released its references. */
static void arr_remove_slot(AnvArr *a, int64_t pos) {
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

/* Byte offset of the first occurrence of t in s at or after `at`, or -1; an empty t is found at
 * once. Looks for t's first byte, then compares the rest. */
static int64_t find_at(const AnvStr *s, const AnvStr *t, int64_t at) {
    if (at + t->len > s->len) return -1;
    if (t->len == 0) return at;
    const char *p = s->bytes + at, *last = s->bytes + s->len - t->len;
    while (p <= last) {
        p = memchr(p, (unsigned char)t->bytes[0], (size_t)(last - p) + 1);
        if (!p) return -1;
        if (memcmp(p + 1, t->bytes + 1, (size_t)t->len - 1) == 0) return p - s->bytes;
        p++;
    }
    return -1;
}

int64_t anvil_str_find(const AnvStr *s, const AnvStr *t) { return find_at(s, t, 0); }

int64_t anvil_str_find_from(const AnvStr *s, const AnvStr *t, int64_t at, const AnvStr *site) {
    if (at < 0 || at > s->len) {
        char buf[512];
        int n = snprintf(buf, sizeof buf, "find from %lld out of bounds for length %lld at %.*s", (long long)at,
                         (long long)s->len, (int)site->len, site->bytes);
        if (n >= (int)sizeof buf) n = (int)sizeof buf - 1;
        panic_bytes(buf, n);
    }
    return find_at(s, t, at);
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
    for (int64_t i = find_at(s, sep, 0); i >= 0; i = find_at(s, sep, start)) {
        *(AnvStr **)anvil_arr_push_slot(out) = str_from(s->bytes + start, i - start);
        start = i + sep->len;
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

/* ---- maps (see AnvMap) ---- */

#define MAP_SMALL 32

AnvArr *anvil_map_new(const AnvTypeInfo *ti, int64_t cap, int64_t key_is_str) {
    AnvMap *m = pool_alloc(sizeof(AnvMap));
    arr_init(&m->a, ti, cap, KIND_MAP, (int64_t)sizeof(AnvMap));
    m->key_is_str = key_is_str;
    m->sorted = 0;
    m->table = NULL;
    m->mask = 0;
    m->last_hash = 0;
    m->last_key = 0;
    m->last_at = INT64_MAX;
    return &m->a;
}

static uint32_t hash_i64(int64_t key) {
    uint64_t h = (uint64_t)key;
    h ^= h >> 32;
    h *= 0x9E3779B97F4A7C15ull;
    return (uint32_t)(h ^ (h >> 29));
}

static uint32_t hash_str(const AnvStr *key) {
    const uint64_t k = 0x9FB21C651E98DF25ull;
    uint64_t h = (uint64_t)key->len * k;
    const char *p = key->bytes;
    int64_t n = key->len;
    for (; n >= 8; n -= 8, p += 8) {
        uint64_t word;
        memcpy(&word, p, 8);
        h = (h ^ word) * k;
        h ^= h >> 47;
    }
    if (n) {
        uint64_t word = 0;
        memcpy(&word, p, (size_t)n);
        h = (h ^ word) * k;
        h ^= h >> 47;
    }
    h *= k;
    return (uint32_t)(h ^ (h >> 32));
}

static uint32_t hash_at(const AnvMap *m, int64_t pos) {
    const char *key = m->a.data + pos * m->a.ti->elem_size;
    return m->key_is_str ? hash_str(*(const AnvStr *const *)key) : hash_i64(*(const int64_t *)key);
}

static int key_before(const AnvMap *m, int64_t i, int64_t j) {
    const char *a = m->a.data + i * m->a.ti->elem_size, *b = m->a.data + j * m->a.ti->elem_size;
    if (m->key_is_str) return anvil_str_cmp(*(const AnvStr *const *)a, *(const AnvStr *const *)b) < 0;
    return *(const int64_t *)a < *(const int64_t *)b;
}

static void table_put(MapSlot *table, int64_t mask, uint32_t hash, uint32_t where) {
    int64_t i = (int64_t)hash & mask;
    while (table[i].where) i = (i + 1) & mask;
    table[i].hash = hash;
    table[i].where = where;
}

static MapSlot *table_new(int64_t slots) {
    mem_note(slots * (int64_t)sizeof(MapSlot), slots * (int64_t)sizeof(MapSlot));
    MapSlot *table = xmalloc((size_t)slots * sizeof(MapSlot));
    memset(table, 0, (size_t)slots * sizeof(MapSlot));
    return table;
}

/* Table with room for twice the elements. */
static void table_grow(AnvMap *m) {
    int64_t slots = (m->mask + 1) * 2;
    MapSlot *table = table_new(slots);
    for (int64_t i = 0; i <= m->mask; i++)
        if (m->table[i].where) table_put(table, slots - 1, m->table[i].hash, m->table[i].where);
    mem_note(0, -(m->mask + 1) * (int64_t)sizeof(MapSlot));
    free(m->table);
    m->table = table;
    m->mask = slots - 1;
}

/* A copy of the map without its elements' contents; the caller fills `len` elements. */
static AnvMap *map_clone(const AnvMap *m) {
    AnvMap *b = (AnvMap *)anvil_map_new(m->a.ti, m->a.len, m->key_is_str);
    b->sorted = m->sorted;
    if (m->table) {
        b->table = table_new(m->mask + 1);
        memcpy(b->table, m->table, (size_t)(m->mask + 1) * sizeof(MapSlot));
        b->mask = m->mask;
    }
    return b;
}

/* Index of `key`, or a negative number: -(insertion point) - 1 while the map is small. */
int64_t anvil_map_find_i64(const AnvArr *a, int64_t key) {
    AnvMap *m = (AnvMap *)a;
    int64_t size = a->ti->elem_size;
    if (m->table) {
        /* Keys are unique, so whatever else has happened, a key found at that place is the one. */
        if (key == m->last_key && m->last_at < a->len && *(const int64_t *)(a->data + m->last_at * size) == key)
            return m->last_at;
        uint32_t h = m->last_hash = hash_i64(key);
        m->last_key = key;
        m->last_at = INT64_MAX; /* a key not found has no place: last_hash is only theirs */
        for (int64_t i = (int64_t)h & m->mask;; i = (i + 1) & m->mask) {
            uint32_t where = m->table[i].where;
            if (!where) return -1;
            if (m->table[i].hash == h && *(const int64_t *)(a->data + (int64_t)(where - 1) * size) == key) return m->last_at = where - 1;
        }
    }
    int64_t lo = 0, hi = a->len;
    while (lo < hi) {
        int64_t mid = lo + (hi - lo) / 2;
        int64_t k = *(const int64_t *)(a->data + mid * size);
        if (k == key) return mid;
        if (k < key) lo = mid + 1;
        else hi = mid;
    }
    return -lo - 1;
}

int64_t anvil_map_find_str(const AnvArr *a, const AnvStr *key) {
    AnvMap *m = (AnvMap *)a;
    int64_t size = a->ti->elem_size;
    if (m->table) {
        /* The same object as last time, which may since be another text: the comparison decides. */
        if ((int64_t)(intptr_t)key == m->last_key && m->last_at < a->len && anvil_str_eq(*(const AnvStr *const *)(a->data + m->last_at * size), key))
            return m->last_at;
        uint32_t h = m->last_hash = hash_str(key);
        m->last_key = (int64_t)(intptr_t)key;
        m->last_at = INT64_MAX;
        for (int64_t i = (int64_t)h & m->mask;; i = (i + 1) & m->mask) {
            uint32_t where = m->table[i].where;
            if (!where) return -1;
            if (m->table[i].hash == h && anvil_str_eq(*(const AnvStr *const *)(a->data + (int64_t)(where - 1) * size), key))
                return m->last_at = where - 1;
        }
    }
    int64_t lo = 0, hi = a->len;
    while (lo < hi) {
        int64_t mid = lo + (hi - lo) / 2;
        int64_t c = anvil_str_cmp(*(const AnvStr *const *)(a->data + mid * size), key);
        if (c == 0) return mid;
        if (c < 0) lo = mid + 1;
        else hi = mid;
    }
    return -lo - 1;
}

/* A slot for a new key, right after a find on this map did not find it (`pos` from that find);
 * the caller fills it. `key` is the key (an i64, or a str pointer). */
char *anvil_map_insert_slot(AnvArr *a, int64_t pos, int64_t key) {
    AnvMap *m = (AnvMap *)a;
    uint32_t hash;
    if (!m->table) {
        if (a->len < MAP_SMALL) return arr_insert_slot(a, pos);
        int64_t slots = 128;
        while (slots < a->len * 4) slots *= 2;
        m->table = table_new(slots);
        m->mask = slots - 1;
        for (int64_t i = 0; i < a->len; i++) table_put(m->table, m->mask, hash_at(m, i), (uint32_t)(i + 1));
        m->sorted = a->len;
        hash = m->key_is_str ? hash_str((const AnvStr *)(intptr_t)key) : hash_i64(key);
    } else {
        hash = m->last_hash;
    }
    if (a->len >= 0xFFFFFFFELL) runtime_error("map too large");
    if ((a->len + 1) * 2 > m->mask + 1) table_grow(m);
    int64_t at = a->len;
    if (m->sorted == at) {
        const char *last = at ? a->data + (at - 1) * a->ti->elem_size : NULL;
        int after = !last || (m->key_is_str ? anvil_str_cmp(*(const AnvStr *const *)last, (const AnvStr *)(intptr_t)key) < 0
                                            : *(const int64_t *)last < key);
        if (after) m->sorted = at + 1;
    }
    table_put(m->table, m->mask, hash, (uint32_t)(at + 1));
    return anvil_arr_push_slot(a);
}

/* The table place of element `pos`, whose key has `hash`. */
static int64_t table_place(const AnvMap *m, uint32_t hash, int64_t pos) {
    for (int64_t i = (int64_t)hash & m->mask;; i = (i + 1) & m->mask)
        if (m->table[i].where == (uint32_t)(pos + 1)) return i;
}

/* Removes element `pos`, right after a find on this map found it; the caller has released its
 * references. */
void anvil_map_remove_slot(AnvArr *a, int64_t pos) {
    AnvMap *m = (AnvMap *)a;
    if (!m->table) {
        arr_remove_slot(a, pos);
        return;
    }
    if (a->rc != 1 || pos < 0 || pos >= a->len) runtime_error("remove from a shared map or outside it");
    /* Empty its place, moving later entries of the run back so a search still reaches them. */
    int64_t hole = table_place(m, m->last_hash, pos), j = hole;
    for (;;) {
        j = (j + 1) & m->mask;
        if (!m->table[j].where) break;
        int64_t home = (int64_t)m->table[j].hash & m->mask;
        if (((j - home) & m->mask) >= ((j - hole) & m->mask)) {
            m->table[hole] = m->table[j];
            hole = j;
        }
    }
    m->table[hole].where = 0;
    /* The last element fills the gap. */
    int64_t last = a->len - 1;
    if (pos != last) {
        int64_t size = a->ti->elem_size;
        m->table[table_place(m, hash_at(m, last), last)].where = (uint32_t)(pos + 1);
        memcpy(a->data + pos * size, a->data + last * size, (size_t)size);
    }
    a->len = last;
    if (m->sorted > pos) m->sorted = pos;
    m->last_at = INT64_MAX; /* another key is there now, and last_hash is not its hash */
}

static void sort_positions(const AnvMap *m, int64_t *order, int64_t *room, int64_t n) {
    if (n < 2) return;
    int64_t half = n / 2, i = 0, j = half, k = 0;
    sort_positions(m, order, room, half);
    sort_positions(m, order + half, room, n - half);
    while (i < half && j < n) room[k++] = key_before(m, order[j], order[i]) ? order[j++] : order[i++];
    while (i < half) room[k++] = order[i++];
    while (j < n) room[k++] = order[j++];
    memcpy(order, room, (size_t)n * sizeof(int64_t));
}

/* Puts the elements in key order. Changes nothing a program can see, so a shared map may be
 * settled; reference counts stay as they are. */
void anvil_map_settle(AnvArr *a) {
    AnvMap *m = (AnvMap *)a;
    if (!m->table) return;
    int64_t n = a->len, s = m->sorted;
    while (s < n && (s == 0 || key_before(m, s - 1, s))) s++;
    m->sorted = n;
    if (s == n) return;
    m->last_at = INT64_MAX; /* keys are about to change places */
    /* The new elements in order, then merged with the old ones. */
    int64_t t = n - s, size = a->ti->elem_size;
    int64_t *tail = xmalloc((size_t)(t * 2) * sizeof(int64_t));
    for (int64_t i = 0; i < t; i++) tail[i] = s + i;
    sort_positions(m, tail, tail + t, t);
    int64_t *order = xmalloc((size_t)n * sizeof(int64_t));
    for (int64_t i = 0, j = 0, k = 0; k < n; k++) {
        if (i < s && (j == t || key_before(m, i, tail[j]))) order[k] = i++;
        else order[k] = tail[j++];
    }
    char *data = pool_alloc((size_t)(a->cap * size));
    uint32_t *place = xmalloc((size_t)n * sizeof(uint32_t));
    for (int64_t k = 0; k < n; k++) {
        memcpy(data + k * size, a->data + order[k] * size, (size_t)size);
        place[order[k]] = (uint32_t)(k + 1);
    }
    for (int64_t i = 0; i <= m->mask; i++)
        if (m->table[i].where) m->table[i].where = place[m->table[i].where - 1];
    pool_free(a->data, (size_t)(a->cap * size));
    a->data = data;
    free(place);
    free(order);
    free(tail);
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
            /* Text is mostly ASCII: eight bytes at a time until one is not. */
            while (i + 8 <= len) {
                uint64_t word;
                memcpy(&word, s + i, 8);
                if (word & 0x8080808080808080ull) break;
                i += 8;
            }
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
static void init_stdin(void) {
    static int ready = 0;
    if (!ready) {
        ready = 1;
#ifdef _WIN32
        _setmode(_fileno(stdin), _O_BINARY);
#endif
    }
    init_stdout();
    fflush(stdout); /* a prompt shows before the program waits */
}

/* The size of a file just opened, or 0 when it cannot say. Not for stdin: moving around in it can
 * lose what is buffered. */
static size_t size_to_read(FILE *f) {
    int64_t end = 0;
#ifdef _WIN32
    if (_fseeki64(f, 0, SEEK_END) == 0) end = _ftelli64(f);
    if (_fseeki64(f, 0, SEEK_SET) != 0) runtime_error("cannot rewind a file");
#else
    if (fseeko(f, 0, SEEK_END) == 0) end = (int64_t)ftello(f);
    if (fseeko(f, 0, SEEK_SET) != 0) runtime_error("cannot rewind a file");
#endif
    clearerr(f);
    return end > 0 ? (size_t)end : 0;
}

/* All that is left to read from `f` as new text, read straight into it: with `expected` right, no
 * growing, and no second copy of what may be most of the program's memory. */
static AnvStr *read_rest(FILE *f, size_t expected) {
    size_t cap = expected ? expected + 1 : 4096, len = 0;
    AnvStr *s = xmalloc(sizeof(AnvStr) + cap + 1);
    for (;;) {
        if (len == cap) {
            cap *= 2;
            AnvStr *grown = realloc(s, sizeof(AnvStr) + cap + 1);
            if (!grown) runtime_error("out of memory");
            s = grown;
        }
        size_t got = fread(s->bytes + len, 1, cap - len, f);
        len += got;
        if (got == 0) break;
    }
    /* Too small for the pools' sizes to matter only above their largest: anvil_release frees those. */
    if (sizeof(AnvStr) + len + 1 <= POOL_LARGEST) {
        AnvStr *small = str_from(s->bytes, (int64_t)len);
        free(s);
        return small;
    }
    s->rc = 1;
    s->kind = KIND_STR;
    s->len = (int64_t)len;
    s->bytes[len] = 0;
    mem_object((int64_t)sizeof(AnvStr) + (int64_t)len + 1);
    COUNT_OBJECTS(1);
    return s;
}

AnvStr *anvil_read_stdin(void) {
    init_stdin();
    fflush(stdout); /* a prompt shows before the program waits */
    return read_rest(stdin, 0);
}

AnvStr *anvil_read_line(const AnvStr *message) {
    init_stdin();
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
    AnvStr *s = read_rest(f, size_to_read(f));
    int failed = ferror(f);
    fclose(f);
    if (failed) last_file_error = FILE_FAILED;
    else if (text && !utf8_valid((const unsigned char *)s->bytes, s->len)) last_file_error = FILE_NOT_TEXT;
    else return s;
    anvil_release((AnvObj *)s);
    return NULL;
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
    if (KIND_OF(o) == KIND_ARR || KIND_OF(o) == KIND_MAP) {
        const AnvArr *a = (const AnvArr *)o;
        AnvArr *b = KIND_OF(o) == KIND_MAP ? &map_clone((const AnvMap *)a)->a : anvil_arr_new(a->ti, a->len);
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
    guard_stack();
    job->work(job->f, job->in, job->out, job->base);
    current_job = NULL;
    pool_give_back();
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
    init_sampling();
    init_stack_guard();
    guard_stack();
    anvil_program();
    return 0;
}
#else
static void *run_program(void *unused) {
    (void)unused;
    guard_stack();
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
    init_sampling();
    init_stack_guard();
    pthread_attr_t attr;
    pthread_t thread;
    if (pthread_attr_init(&attr) != 0 || pthread_attr_setstacksize(&attr, (size_t)64 << 20) != 0 ||
        pthread_create(&thread, &attr, run_program, NULL) != 0)
        runtime_error("cannot start the program's thread");
    pthread_join(thread, NULL);
    return 0;
}
#endif
