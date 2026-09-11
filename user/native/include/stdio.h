/* Standard input and output, over what this system has instead of files.
 *
 * Three kinds of stream exist and nothing else. The diagnostic plane is where
 * `stdout` and `stderr` go: a debugging channel, eight bytes a note, from which
 * nothing durable is derived. A memory sink is what `snprintf` writes into.
 * And a read-only file is a region a supervisor mapped into this domain under
 * a name the target contract fixes: `/bulk`, the sealed bulk object, when the
 * domain was given one. That is the whole filesystem, and it is a fact about
 * the domain's capability table rather than a policy of this library: there is
 * nothing else mapped to open.
 *
 * Opening a name that is not bound is `ENOENT`; opening for writing is
 * `EROFS`. `stdin` exists and is always at end of file.
 */
#ifndef _STDIO_H
#define _STDIO_H

#include <stddef.h>
#include <stdarg.h>
#include <sys/types.h>
#include "thalyx/cdefs.h"

typedef struct th_file FILE;
/* glibc's size, sixteen bytes: an offset and a conversion state. */
typedef struct { long __pos; unsigned long long __state; } fpos_t;

#define EOF (-1)
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
#define BUFSIZ 8192
#define FILENAME_MAX 4096
#define FOPEN_MAX 16
#define L_tmpnam 20
#define TMP_MAX 238328
#define _IOFBF 0
#define _IOLBF 1
#define _IONBF 2

/* The name the bulk region is opened by, when the domain has one. */
#define TH_BULK_PATH "/bulk"

__TH_BEGIN_DECLS

extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;
#define stdin stdin
#define stdout stdout
#define stderr stderr

FILE  *fopen(const char *path, const char *mode);
FILE  *freopen(const char *path, const char *mode, FILE *stream);
FILE  *fdopen(int fd, const char *mode) __TH_NOTHROW;
int    fclose(FILE *stream);
int    fflush(FILE *stream);
size_t fread(void *into, size_t size, size_t count, FILE *stream);
size_t fwrite(const void *data, size_t size, size_t count, FILE *stream);
int    fseek(FILE *stream, long offset, int whence);
long   ftell(FILE *stream);
int    fseeko(FILE *stream, off_t offset, int whence);
off_t  ftello(FILE *stream);
int    fgetpos(FILE *stream, fpos_t *position);
int    fsetpos(FILE *stream, const fpos_t *position);
void   rewind(FILE *stream);
void   clearerr(FILE *stream) __TH_NOTHROW;
int    feof(FILE *stream) __TH_NOTHROW;
int    ferror(FILE *stream) __TH_NOTHROW;
int    fileno(FILE *stream) __TH_NOTHROW;
int    setvbuf(FILE *stream, char *buffer, int mode, size_t size) __TH_NOTHROW;
void   setbuf(FILE *stream, char *buffer) __TH_NOTHROW;
int    fgetc(FILE *stream);
int    getc(FILE *stream);
int    getchar(void);
int    ungetc(int c, FILE *stream);
char  *fgets(char *out, int n, FILE *stream);
int    fputc(int c, FILE *stream);
int    putc(int c, FILE *stream);
int    putchar(int c);
int    fputs(const char *text, FILE *stream);
int    puts(const char *text);
void   perror(const char *prefix);
int    remove(const char *path) __TH_NOTHROW;
int    rename(const char *from, const char *to) __TH_NOTHROW;
FILE  *tmpfile(void);
char  *tmpnam(char *out) __TH_NOTHROW;

int printf(const char *format, ...) __attribute__((__format__(__printf__, 1, 2)));
int fprintf(FILE *stream, const char *format, ...) __attribute__((__format__(__printf__, 2, 3)));
int sprintf(char *out, const char *format, ...) __attribute__((__format__(__printf__, 2, 3)));
int snprintf(char *out, size_t size, const char *format, ...)
    __attribute__((__format__(__printf__, 3, 4)));
int vprintf(const char *format, va_list args);
int vfprintf(FILE *stream, const char *format, va_list args);
int vsprintf(char *out, const char *format, va_list args);
int vsnprintf(char *out, size_t size, const char *format, va_list args);
int vasprintf(char **out, const char *format, va_list args);
int asprintf(char **out, const char *format, ...) __attribute__((__format__(__printf__, 2, 3)));

int scanf(const char *format, ...);
int fscanf(FILE *stream, const char *format, ...);
int sscanf(const char *s, const char *format, ...);
int vscanf(const char *format, va_list args);
int vfscanf(FILE *stream, const char *format, va_list args);
int vsscanf(const char *s, const char *format, va_list args);

__TH_END_DECLS

#endif
