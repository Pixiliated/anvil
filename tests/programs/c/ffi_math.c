/* C functions for tests/programs/ffi.anv. */
#include <stdint.h>

int64_t gcd(int64_t a, int64_t b) {
    while (b != 0) {
        int64_t t = a % b;
        a = b;
        b = t;
    }
    return a < 0 ? -a : a;
}

double lerp(double a, double b, double t) { return a + (b - a) * t; }

int64_t answer(void) { return 42; }

double fold6(int64_t a, double b, int64_t c, double d, int64_t e, double f) {
    return (double)a + b * 10.0 + (double)c * 100.0 + d * 1000.0 + (double)e * 10000.0 + f * 100000.0;
}
