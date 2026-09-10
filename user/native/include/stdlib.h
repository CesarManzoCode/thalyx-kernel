/* Allocation, conversion and leaving.
 *
 * `malloc` is the native heap of `thalyx/nrt.h`: memory objects the program
 * created against its own scope and mapped into its own address space. There
 * is no `brk`, no `mmap` with ambient authority, and no allocator that can
 * outgrow what the scope's ceiling permits -- which is the whole point, since
 * a language runtime's memory limit is only worth stating if the system
 * enforces it too.
 */
#ifndef _STDLIB_H
#define _STDLIB_H
#include <stddef.h>

void  *malloc(size_t n);
void  *calloc(size_t count, size_t size);
void  *realloc(void *p, size_t n);
void   free(void *p);
size_t malloc_usable_size(void *p);

_Noreturn void abort(void);
_Noreturn void exit(int code);

double  strtod(const char *s, char **end);
float   strtof(const char *s, char **end);
long    strtol(const char *s, char **end, int base);
unsigned long strtoul(const char *s, char **end, int base);
long long strtoll(const char *s, char **end, int base);
unsigned long long strtoull(const char *s, char **end, int base);
int     atoi(const char *s);
void    qsort(void *base, size_t count, size_t size, int (*compare)(const void *, const void *));
void   *bsearch(const void *key, const void *base, size_t count, size_t size,
                int (*compare)(const void *, const void *));
char   *getenv(const char *name);

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#endif
