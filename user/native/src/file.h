/* The stream behind `FILE`, shared by the formatting and the file code.
 *
 * Private to the runtime: a program only ever holds a pointer. The first four
 * fields are the order `printf.c` has always used for its memory sink.
 */
#ifndef THALYX_NATIVE_FILE_H
#define THALYX_NATIVE_FILE_H

#include <stddef.h>
#include <stdint.h>

enum {
    TH_FILE_DIAG = 0,    /* the diagnostic plane: stdout and stderr        */
    TH_FILE_MEMORY = 1,  /* a bounded memory sink, for snprintf            */
    TH_FILE_BLOB = 2,    /* a read-only region a supervisor mapped         */
    TH_FILE_EMPTY = 3,   /* stdin: always at its end                       */
};

struct th_file {
    int kind;
    char *out;
    size_t size;
    size_t at;
    const uint8_t *data;
    uint64_t length;
    uint64_t position;
    int fd;
    int eof;
    int error;
    int pushed;          /* a character given back by `ungetc`, or -1      */
    int in_use;
};

/* Writes bytes to a stream, whatever kind it is. Answers the bytes written. */
size_t th_stream_write(struct th_file *stream, const char *bytes, size_t len);

#endif
