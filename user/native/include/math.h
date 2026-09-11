/* Real mathematics for the native target.
 *
 * Every function here is computed, not approximated away: the engine's softmax
 * and the language runtime's number formatting both depend on these being the
 * functions they are named after. What the implementations are, and what their
 * accuracy is, is stated in `user/native/src/math.c`; nothing here claims
 * correctly-rounded results.
 *
 * The whole of C99 is declared, in `double`, `float` and `long double`, because
 * `<cmath>` names all of it. A declaration is not an implementation: a function
 * no program on this system calls is not defined, and calling one is a link
 * error on purpose rather than a plausible wrong answer.
 */
#ifndef _MATH_H
#define _MATH_H

#include "thalyx/cdefs.h"

#define M_E        2.7182818284590452354
#define M_LOG2E    1.4426950408889634074
#define M_LOG10E   0.43429448190325182765
#define M_LN2      0.69314718055994530942
#define M_LN10     2.30258509299404568402
#define M_PI       3.14159265358979323846
#define M_PI_2     1.57079632679489661923
#define M_PI_4     0.78539816339744830962
#define M_1_PI     0.31830988618379067154
#define M_2_PI     0.63661977236758134308
#define M_2_SQRTPI 1.12837916709551257390
#define M_SQRT2    1.41421356237309504880
#define M_SQRT1_2  0.70710678118654752440

#define INFINITY  (__builtin_inff())
#define NAN       (__builtin_nanf(""))
#define HUGE_VAL  (__builtin_inf())
#define HUGE_VALF (__builtin_inff())
#define HUGE_VALL (__builtin_infl())

/* SSE arithmetic evaluates in the declared type. */
typedef float  float_t;
typedef double double_t;

#define FP_NAN       0
#define FP_INFINITE  1
#define FP_ZERO      2
#define FP_SUBNORMAL 3
#define FP_NORMAL    4
#define FP_ILOGB0    (-2147483647 - 1)
#define FP_ILOGBNAN  (-2147483647 - 1)
#define MATH_ERRNO     1
#define MATH_ERREXCEPT 2
#define math_errhandling (MATH_ERRNO | MATH_ERREXCEPT)

#define isnan(x)      __builtin_isnan(x)
#define isinf(x)      __builtin_isinf_sign(x)
#define isfinite(x)   __builtin_isfinite(x)
#define isnormal(x)   __builtin_isnormal(x)
#define signbit(x)    __builtin_signbit(x)
#define fpclassify(x) __builtin_fpclassify(FP_NAN, FP_INFINITE, FP_NORMAL, FP_SUBNORMAL, FP_ZERO, x)
#define isgreater(a, b)      __builtin_isgreater(a, b)
#define isgreaterequal(a, b) __builtin_isgreaterequal(a, b)
#define isless(a, b)         __builtin_isless(a, b)
#define islessequal(a, b)    __builtin_islessequal(a, b)
#define islessgreater(a, b)  __builtin_islessgreater(a, b)
#define isunordered(a, b)    __builtin_isunordered(a, b)

__TH_BEGIN_DECLS

#define __TH_MATH1(name) \
    double name(double x) __TH_NOTHROW; \
    float name##f(float x) __TH_NOTHROW; \
    long double name##l(long double x) __TH_NOTHROW;
#define __TH_MATH2(name) \
    double name(double x, double y) __TH_NOTHROW; \
    float name##f(float x, float y) __TH_NOTHROW; \
    long double name##l(long double x, long double y) __TH_NOTHROW;

__TH_MATH1(acos)
__TH_MATH1(asin)
__TH_MATH1(atan)
__TH_MATH2(atan2)
__TH_MATH1(cos)
__TH_MATH1(sin)
__TH_MATH1(tan)
__TH_MATH1(acosh)
__TH_MATH1(asinh)
__TH_MATH1(atanh)
__TH_MATH1(cosh)
__TH_MATH1(sinh)
__TH_MATH1(tanh)
__TH_MATH1(exp)
__TH_MATH1(exp2)
__TH_MATH1(expm1)
__TH_MATH1(log)
__TH_MATH1(log10)
__TH_MATH1(log1p)
__TH_MATH1(log2)
__TH_MATH1(logb)
__TH_MATH1(cbrt)
__TH_MATH1(fabs)
__TH_MATH1(sqrt)
__TH_MATH1(erf)
__TH_MATH1(erfc)
__TH_MATH1(lgamma)
__TH_MATH1(tgamma)
__TH_MATH1(ceil)
__TH_MATH1(floor)
__TH_MATH1(nearbyint)
__TH_MATH1(rint)
__TH_MATH1(round)
__TH_MATH1(trunc)
__TH_MATH2(fmod)
__TH_MATH2(remainder)
__TH_MATH2(copysign)
__TH_MATH2(nextafter)
__TH_MATH2(fdim)
__TH_MATH2(fmax)
__TH_MATH2(fmin)
__TH_MATH2(hypot)
__TH_MATH2(pow)

#undef __TH_MATH1
#undef __TH_MATH2

double frexp(double x, int *exponent) __TH_NOTHROW;
float frexpf(float x, int *exponent) __TH_NOTHROW;
long double frexpl(long double x, int *exponent) __TH_NOTHROW;
double ldexp(double x, int exponent) __TH_NOTHROW;
float ldexpf(float x, int exponent) __TH_NOTHROW;
long double ldexpl(long double x, int exponent) __TH_NOTHROW;
double modf(double x, double *integral) __TH_NOTHROW;
float modff(float x, float *integral) __TH_NOTHROW;
long double modfl(long double x, long double *integral) __TH_NOTHROW;
double scalbn(double x, int exponent) __TH_NOTHROW;
float scalbnf(float x, int exponent) __TH_NOTHROW;
long double scalbnl(long double x, int exponent) __TH_NOTHROW;
double scalbln(double x, long exponent) __TH_NOTHROW;
float scalblnf(float x, long exponent) __TH_NOTHROW;
long double scalblnl(long double x, long exponent) __TH_NOTHROW;
int ilogb(double x) __TH_NOTHROW;
int ilogbf(float x) __TH_NOTHROW;
int ilogbl(long double x) __TH_NOTHROW;
double fma(double x, double y, double z) __TH_NOTHROW;
float fmaf(float x, float y, float z) __TH_NOTHROW;
long double fmal(long double x, long double y, long double z) __TH_NOTHROW;
double remquo(double x, double y, int *quotient) __TH_NOTHROW;
float remquof(float x, float y, int *quotient) __TH_NOTHROW;
long double remquol(long double x, long double y, int *quotient) __TH_NOTHROW;
double nexttoward(double x, long double y) __TH_NOTHROW;
float nexttowardf(float x, long double y) __TH_NOTHROW;
long double nexttowardl(long double x, long double y) __TH_NOTHROW;
double nan(const char *tag) __TH_NOTHROW;
float nanf(const char *tag) __TH_NOTHROW;
long double nanl(const char *tag) __TH_NOTHROW;
long lrint(double x) __TH_NOTHROW;
long lrintf(float x) __TH_NOTHROW;
long lrintl(long double x) __TH_NOTHROW;
long long llrint(double x) __TH_NOTHROW;
long long llrintf(float x) __TH_NOTHROW;
long long llrintl(long double x) __TH_NOTHROW;
long lround(double x) __TH_NOTHROW;
long lroundf(float x) __TH_NOTHROW;
long lroundl(long double x) __TH_NOTHROW;
long long llround(double x) __TH_NOTHROW;
long long llroundf(float x) __TH_NOTHROW;
long long llroundl(long double x) __TH_NOTHROW;

__TH_END_DECLS

#endif
