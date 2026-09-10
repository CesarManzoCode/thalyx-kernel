/* Formatted output, without files.
 *
 * There is no filesystem here and no descriptor table, so `FILE` is not a file:
 * the two streams that exist are the diagnostic plane, which is where anything
 * a program prints goes, and a memory sink used by `snprintf`. Everything else
 * a ported program asks of `stdio.h` is a link error on purpose.
 */
#ifndef _STDIO_H
#define _STDIO_H
#include <stddef.h>
#include <stdarg.h>

typedef struct th_file FILE;
extern FILE *th_stdout;
extern FILE *th_stderr;
#define stdout th_stdout
#define stderr th_stderr

int  printf(const char *format, ...) __attribute__((format(printf, 1, 2)));
int  fprintf(FILE *stream, const char *format, ...) __attribute__((format(printf, 2, 3)));
int  vfprintf(FILE *stream, const char *format, va_list args);
int  snprintf(char *out, size_t size, const char *format, ...) __attribute__((format(printf, 3, 4)));
int  vsnprintf(char *out, size_t size, const char *format, va_list args);
int  fputs(const char *text, FILE *stream);
int  fputc(int c, FILE *stream);
int  putchar(int c);
int  puts(const char *text);
size_t fwrite(const void *data, size_t size, size_t count, FILE *stream);
int  fflush(FILE *stream);
#endif
