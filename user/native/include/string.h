/* String and memory, for the native C target.
 *
 * The subset real programs on this system use, and nothing beyond it. A
 * function that is not here is one no ported program has needed yet; adding a
 * stub that returns a plausible value would be worse than a link error.
 */
#ifndef _STRING_H
#define _STRING_H
#include <stddef.h>

void  *memcpy(void *dst, const void *src, size_t n);
void  *memmove(void *dst, const void *src, size_t n);
void  *memset(void *dst, int c, size_t n);
int    memcmp(const void *a, const void *b, size_t n);
void  *memchr(const void *s, int c, size_t n);
size_t strlen(const char *s);
size_t strnlen(const char *s, size_t n);
char  *strcpy(char *dst, const char *src);
char  *strncpy(char *dst, const char *src, size_t n);
char  *strcat(char *dst, const char *src);
int    strcmp(const char *a, const char *b);
int    strncmp(const char *a, const char *b, size_t n);
char  *strchr(const char *s, int c);
char  *strrchr(const char *s, int c);
char  *strstr(const char *h, const char *n);
char  *strdup(const char *s);
char  *strerror(int errnum);
#endif
