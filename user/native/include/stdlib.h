/* General utilities.
 *
 * Allocation is the native heap (`heap.c`): memory objects the program created
 * against its own scope. `getenv` answers nothing because there is no
 * environment, and `system` refuses because there is nothing to run a command
 * with; neither pretends otherwise.
 */
#ifndef _STDLIB_H
#define _STDLIB_H

#include <stddef.h>
#include "thalyx/cdefs.h"

__TH_BEGIN_DECLS

typedef struct { int quot; int rem; } div_t;
typedef struct { long quot; long rem; } ldiv_t;
typedef struct { long long quot; long long rem; } lldiv_t;

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#define RAND_MAX 2147483647

/* The C locale is one byte per character, and so is every locale here. The
 * macro calls the function glibc's does, because code compiled against glibc's
 * headers calls it too. */
size_t __ctype_get_mb_cur_max(void) __TH_NOTHROW;
#define MB_CUR_MAX (__ctype_get_mb_cur_max())

void  *malloc(size_t n) __TH_NOTHROW;
void  *calloc(size_t count, size_t size) __TH_NOTHROW;
void  *realloc(void *p, size_t n) __TH_NOTHROW;
void   free(void *p) __TH_NOTHROW;
void  *aligned_alloc(size_t alignment, size_t size) __TH_NOTHROW;
int    posix_memalign(void **out, size_t alignment, size_t size) __TH_NOTHROW;
size_t malloc_usable_size(void *p) __TH_NOTHROW;

void   abort(void) __TH_NOTHROW __TH_NORETURN;
void   exit(int code) __TH_NOTHROW __TH_NORETURN;
void   _Exit(int code) __TH_NOTHROW __TH_NORETURN;
void   quick_exit(int code) __TH_NOTHROW __TH_NORETURN;
int    atexit(void (*fn)(void)) __TH_NOTHROW;
int    at_quick_exit(void (*fn)(void)) __TH_NOTHROW;

double    atof(const char *s) __TH_NOTHROW;
int       atoi(const char *s) __TH_NOTHROW;
long      atol(const char *s) __TH_NOTHROW;
long long atoll(const char *s) __TH_NOTHROW;
double      strtod(const char *s, char **end) __TH_NOTHROW;
float       strtof(const char *s, char **end) __TH_NOTHROW;
long double strtold(const char *s, char **end) __TH_NOTHROW;
long               strtol(const char *s, char **end, int base) __TH_NOTHROW;
unsigned long      strtoul(const char *s, char **end, int base) __TH_NOTHROW;
long long          strtoll(const char *s, char **end, int base) __TH_NOTHROW;
unsigned long long strtoull(const char *s, char **end, int base) __TH_NOTHROW;

int       abs(int value) __TH_NOTHROW __attribute__((__const__));
long      labs(long value) __TH_NOTHROW __attribute__((__const__));
long long llabs(long long value) __TH_NOTHROW __attribute__((__const__));
div_t     div(int numerator, int denominator) __TH_NOTHROW __attribute__((__const__));
ldiv_t    ldiv(long numerator, long denominator) __TH_NOTHROW __attribute__((__const__));
lldiv_t   lldiv(long long numerator, long long denominator) __TH_NOTHROW __attribute__((__const__));

/* Not `noexcept`: the comparison is the caller's, and in C++ it may throw. */
void  qsort(void *base, size_t count, size_t size, int (*compare)(const void *, const void *));
void *bsearch(const void *key, const void *base, size_t count, size_t size,
              int (*compare)(const void *, const void *));

int   rand(void) __TH_NOTHROW;
void  srand(unsigned seed) __TH_NOTHROW;
char *getenv(const char *name) __TH_NOTHROW;
int   setenv(const char *name, const char *value, int overwrite) __TH_NOTHROW;
int   unsetenv(const char *name) __TH_NOTHROW;
int   system(const char *command);

int    mblen(const char *s, size_t n) __TH_NOTHROW;
int    mbtowc(wchar_t *out, const char *s, size_t n) __TH_NOTHROW;
int    wctomb(char *out, wchar_t wc) __TH_NOTHROW;
size_t mbstowcs(wchar_t *out, const char *s, size_t n) __TH_NOTHROW;
size_t wcstombs(char *out, const wchar_t *s, size_t n) __TH_NOTHROW;

__TH_END_DECLS

#define alloca(n) __builtin_alloca(n)

#endif
