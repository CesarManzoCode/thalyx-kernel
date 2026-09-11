/* String and memory, for the native target.
 *
 * What real programs on this system use. Collation is the C locale's, which is
 * byte order, and `strerror` has the message for every error number this
 * system can produce.
 */
#ifndef _STRING_H
#define _STRING_H

#include <stddef.h>
#include <strings.h>
#include "thalyx/cdefs.h"

__TH_BEGIN_DECLS
void  *memcpy(void *dst, const void *src, size_t n) __TH_NOTHROW;
void  *memmove(void *dst, const void *src, size_t n) __TH_NOTHROW;
void  *memset(void *dst, int c, size_t n) __TH_NOTHROW;
int    memcmp(const void *a, const void *b, size_t n) __TH_NOTHROW;
void  *memchr(const void *s, int c, size_t n) __TH_NOTHROW;
void  *memrchr(const void *s, int c, size_t n) __TH_NOTHROW;
size_t strlen(const char *s) __TH_NOTHROW;
size_t strnlen(const char *s, size_t n) __TH_NOTHROW;
char  *strcpy(char *dst, const char *src) __TH_NOTHROW;
char  *strncpy(char *dst, const char *src, size_t n) __TH_NOTHROW;
char  *strcat(char *dst, const char *src) __TH_NOTHROW;
char  *strncat(char *dst, const char *src, size_t n) __TH_NOTHROW;
int    strcmp(const char *a, const char *b) __TH_NOTHROW;
int    strncmp(const char *a, const char *b, size_t n) __TH_NOTHROW;
int    strcoll(const char *a, const char *b) __TH_NOTHROW;
size_t strxfrm(char *dst, const char *src, size_t n) __TH_NOTHROW;
char  *strchr(const char *s, int c) __TH_NOTHROW;
char  *strrchr(const char *s, int c) __TH_NOTHROW;
char  *strstr(const char *h, const char *n) __TH_NOTHROW;
char  *strpbrk(const char *s, const char *accept) __TH_NOTHROW;
size_t strspn(const char *s, const char *accept) __TH_NOTHROW;
size_t strcspn(const char *s, const char *reject) __TH_NOTHROW;
char  *strtok(char *s, const char *delim) __TH_NOTHROW;
char  *strtok_r(char *s, const char *delim, char **save) __TH_NOTHROW;
char  *strdup(const char *s) __TH_NOTHROW;
char  *strndup(const char *s, size_t n) __TH_NOTHROW;
char  *strerror(int errnum) __TH_NOTHROW;
/* The GNU form: the answer is the returned pointer, which may or may not be
 * `into`. */
char  *strerror_r(int errnum, char *into, size_t n) __TH_NOTHROW;
__TH_END_DECLS

#endif
