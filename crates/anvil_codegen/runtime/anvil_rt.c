/* Anvil runtime: the only code a compiled Anvil program links besides the C runtime.
 * Every function here has a twin in anvil_interp (format.rs); the parity tests keep them identical. */
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

static void write_bytes(const char *p, int64_t len) {
    init_stdout();
    fwrite(p, 1, (size_t)len, stdout);
}

void anvil_print_str(const char *p, int64_t len) { write_bytes(p, len); }

void anvil_print_newline(void) { write_bytes("\n", 1); }

void anvil_print_i64(int64_t v) {
    char buf[32];
    int n = snprintf(buf, sizeof buf, "%lld", (long long)v);
    write_bytes(buf, n);
}

void anvil_print_bool(int64_t v) {
    if (v) write_bytes("true", 4); else write_bytes("false", 5);
}

/* Shortest digits that round-trip, then Anvil's layout (see docs/SPEC.md, "Printing floats").
 * out must hold 64 bytes. */
int anvil_format_f64(double v, char *out) {
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

void anvil_print_f64(double v) {
    char buf[64];
    int n = anvil_format_f64(v, buf);
    write_bytes(buf, n);
}

void anvil_panic(const char *p, int64_t len) {
    init_stdout();
    fflush(stdout);
    fputs("panic: ", stderr);
    fwrite(p, 1, (size_t)len, stderr);
    fputc('\n', stderr);
    fflush(stderr);
    exit(ANVIL_PANIC_EXIT);
}

/* Called by the generated main after anvil_main returns, so buffered output is never lost. */
void anvil_exit(int64_t code) {
    fflush(stdout);
    exit((int)code);
}

/* main's i64 result becomes the exit code; outside 0..=255 is a panic (same text as ir::main_exit_message). */
void anvil_check_exit(int64_t code) {
    if (code < 0 || code > 255) {
        char buf[128];
        int n = snprintf(buf, sizeof buf, "main returned %lld, outside the exit code range 0..=255", (long long)code);
        anvil_panic(buf, n);
    }
}
