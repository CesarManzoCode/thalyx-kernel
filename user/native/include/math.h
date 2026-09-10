/* Real double-precision mathematics for the native target.
 *
 * Every function here is computed, not approximated away: the engine's softmax
 * and the language runtime's number formatting both depend on these being the
 * functions they are named after. What the implementations are, and what their
 * accuracy is, is stated in `user/native/src/math.c`; nothing here claims
 * correctly-rounded results.
 */
#ifndef _MATH_H
#define _MATH_H

#define M_PI   3.14159265358979323846
#define M_E    2.7182818284590452354
#define M_LN2  0.69314718055994530942
#define M_LN10 2.30258509299404568402
#define M_LOG2E 1.4426950408889634074
#define M_SQRT2 1.41421356237309504880

#define INFINITY (__builtin_inff())
#define NAN      (__builtin_nanf(""))
#define HUGE_VAL (__builtin_inf())
#define HUGE_VALF (__builtin_inff())

#define FP_NAN 0
#define FP_INFINITE 1
#define FP_ZERO 2
#define FP_SUBNORMAL 3
#define FP_NORMAL 4

#define isnan(x)      __builtin_isnan(x)
#define isinf(x)      __builtin_isinf(x)
#define isfinite(x)   __builtin_isfinite(x)
#define signbit(x)    __builtin_signbit(x)
#define fpclassify(x) __builtin_fpclassify(FP_NAN, FP_INFINITE, FP_NORMAL, FP_SUBNORMAL, FP_ZERO, x)
#define isgreater(a, b) __builtin_isgreater(a, b)
#define isless(a, b)    __builtin_isless(a, b)

double fabs(double x);
double sqrt(double x);
double floor(double x);
double ceil(double x);
double trunc(double x);
double round(double x);
double rint(double x);
double nearbyint(double x);
double fmod(double x, double y);
double exp(double x);
double expm1(double x);
double log(double x);
double log2(double x);
double log10(double x);
double log1p(double x);
double pow(double x, double y);
double sin(double x);
double cos(double x);
double tan(double x);
double asin(double x);
double acos(double x);
double atan(double x);
double atan2(double y, double x);
double sinh(double x);
double cosh(double x);
double tanh(double x);
double asinh(double x);
double acosh(double x);
double atanh(double x);
double cbrt(double x);
double hypot(double x, double y);
double ldexp(double x, int exponent);
double frexp(double x, int *exponent);
double modf(double x, double *integral);
double copysign(double x, double y);
double fmin(double x, double y);
double fmax(double x, double y);
double nan(const char *tag);
double scalbn(double x, int exponent);

float fabsf(float x);
float sqrtf(float x);
float expf(float x);
float logf(float x);
float powf(float x, float y);
float floorf(float x);
float ceilf(float x);
float fmodf(float x, float y);
float copysignf(float x, float y);
float tanhf(float x);
float sinf(float x);
float cosf(float x);
#endif
