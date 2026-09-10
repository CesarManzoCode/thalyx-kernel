/* Double-precision mathematics, computed here.
 *
 * There is no libm under this program, so these are written out. They are
 * *not* correctly rounded and this file does not pretend otherwise: the
 * elementary functions are argument reduction plus a minimax polynomial, good
 * to within a few units in the last place over the ranges the ported workloads
 * use, and the accuracy of the last bit is neither claimed nor tested.
 *
 * That matters in exactly one place and it is stated where it is used: the
 * inference engine's softmax and SiLU go through `expf`, so the engine's
 * logits differ from another implementation's in the last bits whatever libm
 * is underneath -- llama.cpp's own vectorised `expf` differs from glibc's for
 * the same reason. What the engine's evidence therefore compares is the token
 * a greedy sampler picks, together with the margin it won by, so that
 * "the same answer" is a claim about a decision and not about a bit pattern.
 */

#include <math.h>
#include <stdint.h>

typedef union { double d; uint64_t u; } bits64;
typedef union { float f; uint32_t u; } bits32;

double fabs(double x) { bits64 b = { x }; b.u &= 0x7FFFFFFFFFFFFFFFull; return b.d; }
float  fabsf(float x) { bits32 b = { x }; b.u &= 0x7FFFFFFFu; return b.f; }

double sqrt(double x)
{
    double out;
    __asm__("sqrtsd %1, %0" : "=x"(out) : "x"(x));
    return out;
}

float sqrtf(float x)
{
    float out;
    __asm__("sqrtss %1, %0" : "=x"(out) : "x"(x));
    return out;
}

double copysign(double x, double y)
{
    bits64 a = { x }, b = { y };
    a.u = (a.u & 0x7FFFFFFFFFFFFFFFull) | (b.u & 0x8000000000000000ull);
    return a.d;
}

float copysignf(float x, float y)
{
    bits32 a = { x }, b = { y };
    a.u = (a.u & 0x7FFFFFFFu) | (b.u & 0x80000000u);
    return a.f;
}

double trunc(double x)
{
    if (!isfinite(x) || fabs(x) >= 4503599627370496.0) { return x; }
    double out = (double)(long long)x;
    return copysign(out, x);
}

double floor(double x)
{
    double t = trunc(x);
    return (t > x) ? t - 1.0 : t;
}

double ceil(double x)
{
    double t = trunc(x);
    return (t < x) ? t + 1.0 : t;
}

double round(double x)
{
    if (!isfinite(x) || fabs(x) >= 4503599627370496.0) { return x; }
    double away = floor(fabs(x) + 0.5);
    return copysign(away, x);
}

/* Round half to even, which is what the default rounding mode means. */
double rint(double x)
{
    if (!isfinite(x) || fabs(x) >= 4503599627370496.0) { return x; }
    double magic = 4503599627370496.0;              /* 2^52 */
    double shifted = fabs(x) + magic;
    shifted -= magic;
    return copysign(shifted, x);
}

double nearbyint(double x) { return rint(x); }

double fmod(double x, double y)
{
    if (isnan(x) || isnan(y) || isinf(x) || y == 0.0) { return NAN; }
    if (isinf(y)) { return x; }
    double a = fabs(x), b = fabs(y);
    if (a < b) { return x; }
    int exponent_a, exponent_b;
    frexp(a, &exponent_a);
    frexp(b, &exponent_b);
    for (int shift = exponent_a - exponent_b; shift >= 0; shift--) {
        double scaled = ldexp(b, shift);
        if (scaled <= a) { a -= scaled; }
    }
    return copysign(a, x);
}

float fmodf(float x, float y) { return (float)fmod((double)x, (double)y); }

double ldexp(double x, int exponent)
{
    if (x == 0.0 || !isfinite(x)) { return x; }
    while (exponent > 1023) { x *= 8.98846567431158e307; exponent -= 1023; }
    while (exponent < -1022) { x *= 2.2250738585072014e-308; exponent += 1022; }
    bits64 scale = { 0 };
    scale.u = (uint64_t)(exponent + 1023) << 52;
    return x * scale.d;
}

double scalbn(double x, int exponent) { return ldexp(x, exponent); }

double frexp(double x, int *exponent)
{
    bits64 b = { x };
    int raw = (int)((b.u >> 52) & 0x7FF);
    if (x == 0.0 || !isfinite(x)) { *exponent = 0; return x; }
    if (raw == 0) {                                  /* subnormal */
        b.d = x * 9007199254740992.0;                /* 2^53 */
        raw = (int)((b.u >> 52) & 0x7FF);
        *exponent = raw - 1022 - 53;
    } else {
        *exponent = raw - 1022;
    }
    b.u = (b.u & 0x800FFFFFFFFFFFFFull) | 0x3FE0000000000000ull;
    return b.d;
}

double modf(double x, double *integral)
{
    double whole = trunc(x);
    *integral = whole;
    if (isinf(x)) { return copysign(0.0, x); }
    return x - whole;
}

double fmin(double x, double y) { if (isnan(x)) { return y; } if (isnan(y)) { return x; } return x < y ? x : y; }
double fmax(double x, double y) { if (isnan(x)) { return y; } if (isnan(y)) { return x; } return x > y ? x : y; }
double nan(const char *tag) { (void)tag; return NAN; }

/* ------------------------------------------------------------------ log */

/* log(x) = 2 * atanh((m-1)/(m+1)) + k*ln2, with m in [sqrt(2)/2, sqrt(2)).
 * The odd series in s converges in eight terms over that interval, and ln2 is
 * split in two so that k*ln2 does not lose the low bits for large k. */
static const double LN2_HI = 6.93147180369123816490e-01;
static const double LN2_LO = 1.90821492927058770002e-10;

double log(double x)
{
    if (isnan(x) || x < 0.0) { return NAN; }
    if (x == 0.0) { return -HUGE_VAL; }
    if (isinf(x)) { return x; }

    int k;
    double m = frexp(x, &k);
    if (m < 0.70710678118654752440) { m *= 2.0; k--; }

    double s = (m - 1.0) / (m + 1.0);
    double s2 = s * s;
    double series = s2 * (1.0 / 3.0 + s2 * (1.0 / 5.0 + s2 * (1.0 / 7.0 + s2 * (1.0 / 9.0
                  + s2 * (1.0 / 11.0 + s2 * (1.0 / 13.0 + s2 * (1.0 / 15.0 + s2 / 17.0)))))));
    double log_m = 2.0 * (s + s * series);
    return (double)k * LN2_HI + ((double)k * LN2_LO + log_m);
}

double log2(double x) { return log(x) * 1.4426950408889634074; }
double log10(double x) { return log(x) * 0.43429448190325182765; }
double log1p(double x)
{
    if (fabs(x) > 1e-4) { return log(1.0 + x); }
    return x - x * x / 2.0 + x * x * x / 3.0 - x * x * x * x / 4.0;
}

/* ------------------------------------------------------------------ exp */

double exp(double x)
{
    if (isnan(x)) { return x; }
    if (x > 709.782712893384) { return HUGE_VAL; }
    if (x < -745.1332191019411) { return 0.0; }

    double kd = rint(x * 1.4426950408889634074);
    int k = (int)kd;
    double r = (x - kd * LN2_HI) - kd * LN2_LO;

    /* Taylor of exp(r) on |r| <= ln2/2; twelve terms are past double here. */
    double sum = 1.0 + r * (1.0 + r * (1.0 / 2.0 + r * (1.0 / 6.0 + r * (1.0 / 24.0
               + r * (1.0 / 120.0 + r * (1.0 / 720.0 + r * (1.0 / 5040.0
               + r * (1.0 / 40320.0 + r * (1.0 / 362880.0 + r / 3628800.0)))))))));
    return ldexp(sum, k);
}

double expm1(double x)
{
    if (fabs(x) > 1e-5) { return exp(x) - 1.0; }
    return x + x * x / 2.0 + x * x * x / 6.0;
}

float expf(float x) { return (float)exp((double)x); }
float logf(float x) { return (float)log((double)x); }

double pow(double x, double y)
{
    if (y == 0.0) { return 1.0; }
    if (isnan(x) || isnan(y)) { return NAN; }
    if (x == 1.0) { return 1.0; }
    if (y == 1.0) { return x; }

    int y_is_integer = (y == trunc(y)) && fabs(y) < 9.007199254740992e15;
    int y_is_odd = y_is_integer && ((long long)y & 1);

    if (x == 0.0) {
        if (y < 0.0) { return y_is_odd ? copysign(HUGE_VAL, x) : HUGE_VAL; }
        return y_is_odd ? x : 0.0;
    }
    if (isinf(x)) {
        if (x > 0.0) { return y > 0.0 ? HUGE_VAL : 0.0; }
        if (y > 0.0) { return y_is_odd ? -HUGE_VAL : HUGE_VAL; }
        return y_is_odd ? -0.0 : 0.0;
    }
    if (isinf(y)) {
        double magnitude = fabs(x);
        if (magnitude == 1.0) { return NAN; }
        return (magnitude > 1.0) == (y > 0.0) ? HUGE_VAL : 0.0;
    }
    if (x < 0.0) {
        if (!y_is_integer) { return NAN; }
        double magnitude = pow(-x, y);
        return y_is_odd ? -magnitude : magnitude;
    }

    /* Small integer exponents by repeated squaring: exact where exp/log is not,
     * and it is the case ordinary code actually writes. */
    if (y_is_integer && fabs(y) <= 1024.0) {
        double base = x, accumulator = 1.0;
        long long power = (long long)fabs(y);
        while (power) {
            if (power & 1) { accumulator *= base; }
            base *= base;
            power >>= 1;
        }
        return y < 0.0 ? 1.0 / accumulator : accumulator;
    }
    return exp(y * log(x));
}

float powf(float x, float y) { return (float)pow((double)x, (double)y); }
float floorf(float x) { return (float)floor((double)x); }
float ceilf(float x) { return (float)ceil((double)x); }

double cbrt(double x)
{
    if (x == 0.0 || !isfinite(x)) { return x; }
    double magnitude = fabs(x);
    double guess = exp(log(magnitude) / 3.0);
    for (int i = 0; i < 3; i++) {                 /* Newton, three rounds is plenty */
        guess = guess - (guess - magnitude / (guess * guess)) / 3.0;
    }
    return copysign(guess, x);
}

double hypot(double x, double y)
{
    x = fabs(x); y = fabs(y);
    if (x < y) { double t = x; x = y; y = t; }
    if (x == 0.0) { return 0.0; }
    double ratio = y / x;
    return x * sqrt(1.0 + ratio * ratio);
}

/* ------------------------------------------------------- trigonometry */

/* pi/2 in three pieces, so reduction keeps its low bits for arguments up to
 * about 2^20. Beyond that this falls back to one `fmod` and the result loses
 * digits; the file header says so and nothing here needs it. */
static const double PIO2_1 = 1.57079632673412561417e+00;
static const double PIO2_2 = 6.07710050650619224932e-11;
static const double PIO2_3 = 2.02226624879595063154e-21;

static double sin_kernel(double x)
{
    double x2 = x * x;
    return x * (1.0 + x2 * (-1.0 / 6.0 + x2 * (1.0 / 120.0 + x2 * (-1.0 / 5040.0
             + x2 * (1.0 / 362880.0 + x2 * (-1.0 / 39916800.0 + x2 / 6227020800.0))))));
}

static double cos_kernel(double x)
{
    double x2 = x * x;
    return 1.0 + x2 * (-1.0 / 2.0 + x2 * (1.0 / 24.0 + x2 * (-1.0 / 720.0
             + x2 * (1.0 / 40320.0 + x2 * (-1.0 / 3628800.0 + x2 / 479001600.0)))));
}

static int reduce(double x, double *out)
{
    double quotient = rint(x * 0.63661977236758134308);   /* 2/pi */
    double r = ((x - quotient * PIO2_1) - quotient * PIO2_2) - quotient * PIO2_3;
    *out = r;
    long long n = (long long)quotient;
    return (int)(n & 3);
}

double sin(double x)
{
    if (!isfinite(x)) { return NAN; }
    double r;
    switch (reduce(x, &r)) {
    case 0: return sin_kernel(r);
    case 1: return cos_kernel(r);
    case 2: return -sin_kernel(r);
    default: return -cos_kernel(r);
    }
}

double cos(double x)
{
    if (!isfinite(x)) { return NAN; }
    double r;
    switch (reduce(x, &r)) {
    case 0: return cos_kernel(r);
    case 1: return -sin_kernel(r);
    case 2: return -cos_kernel(r);
    default: return sin_kernel(r);
    }
}

float sinf(float x) { return (float)sin((double)x); }
float cosf(float x) { return (float)cos((double)x); }

double tan(double x)
{
    double s = sin(x), c = cos(x);
    if (c == 0.0) { return copysign(HUGE_VAL, s); }
    return s / c;
}

double atan(double x)
{
    if (isnan(x)) { return x; }
    int negate = 0, invert = 0;
    if (x < 0.0) { x = -x; negate = 1; }
    if (x > 1.0) { x = 1.0 / x; invert = 1; }

    /* One more reduction with tan(pi/12), so the series argument stays under
     * 0.27 and eleven terms are enough. */
    int shifted = 0;
    if (x > 0.26794919243112270647) {
        x = (x * 1.7320508075688772935 - 1.0) / (1.7320508075688772935 + x);
        shifted = 1;
    }

    double x2 = x * x;
    double result = x * (1.0 + x2 * (-1.0 / 3.0 + x2 * (1.0 / 5.0 + x2 * (-1.0 / 7.0
                  + x2 * (1.0 / 9.0 + x2 * (-1.0 / 11.0 + x2 * (1.0 / 13.0
                  + x2 * (-1.0 / 15.0 + x2 * (1.0 / 17.0 + x2 * (-1.0 / 19.0 + x2 / 21.0))))))))));
    if (shifted) { result += 0.52359877559829887308; }   /* pi/6 */
    if (invert) { result = 1.57079632679489661923 - result; }
    return negate ? -result : result;
}

double atan2(double y, double x)
{
    if (isnan(x) || isnan(y)) { return NAN; }
    if (x == 0.0 && y == 0.0) { return signbit(x) ? copysign(M_PI, y) : copysign(0.0, y); }
    if (x == 0.0) { return copysign(1.57079632679489661923, y); }
    double base = atan(y / x);
    if (x > 0.0) { return base; }
    return y >= 0.0 ? base + M_PI : base - M_PI;
}

double asin(double x)
{
    if (isnan(x)) { return x; }
    if (x > 1.0 || x < -1.0) { return NAN; }
    if (x == 1.0) { return 1.57079632679489661923; }
    if (x == -1.0) { return -1.57079632679489661923; }
    return atan(x / sqrt(1.0 - x * x));
}

double acos(double x)
{
    if (isnan(x)) { return x; }
    if (x > 1.0 || x < -1.0) { return NAN; }
    return 1.57079632679489661923 - asin(x);
}

double sinh(double x) { return (exp(x) - exp(-x)) / 2.0; }
double cosh(double x) { return (exp(x) + exp(-x)) / 2.0; }

double tanh(double x)
{
    if (isnan(x)) { return x; }
    if (x > 20.0) { return 1.0; }
    if (x < -20.0) { return -1.0; }
    double e = exp(2.0 * x);
    return (e - 1.0) / (e + 1.0);
}

float tanhf(float x) { return (float)tanh((double)x); }

double asinh(double x) { return copysign(log(fabs(x) + sqrt(x * x + 1.0)), x); }
double acosh(double x) { return x < 1.0 ? NAN : log(x + sqrt(x * x - 1.0)); }
double atanh(double x)
{
    if (fabs(x) > 1.0) { return NAN; }
    if (x == 1.0) { return HUGE_VAL; }
    if (x == -1.0) { return -HUGE_VAL; }
    return 0.5 * log((1.0 + x) / (1.0 - x));
}
