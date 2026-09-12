/* Files, where the only files are regions a supervisor mapped read-only.
 *
 * `vault/roadmap/phases.md` puts a managed compatibility filesystem in K5, and
 * this is the part of one a native program on this system can honestly have.
 * A name is bound only when the domain was given the region it names, and the
 * binding is the target contract, not the program's choice: the sealed bulk
 * object mapped at `TH_BULK_VADDR` is `/bulk`. So "the engine cannot open
 * anything but its weights" is a fact about what was mapped into it.
 *
 * Reading is copying out of that mapping, and `mmap` is the same fact without
 * the copy: the region is already mapped, read-only, at an address this domain
 * was given, so mapping it read-only answers with that address and nothing
 * moves. That is the whole of what `mmap` does here. It allocates nothing, it
 * chooses no address, it grants no authority the domain did not already hold,
 * and every other request -- a write mapping, an anonymous one, a non-zero
 * offset, a fixed address -- is refused rather than emulated with a copy.
 * Writing is refused with `EROFS` and an unbound name with `ENOENT`; neither is
 * accepted and dropped.
 *
 * Descriptors 0, 1 and 2 are the standard streams. An opened file is
 * descriptor 3 onwards, backed by the same stream `fopen` would return.
 */

#include "thalyx/nrt.h"
#include "file.h"
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdarg.h>
#include <sys/stat.h>
#include <sys/mman.h>

typedef struct {
    const char *name;
    const uint8_t *data;
    uint64_t length;
} th_bound;

#define TH_BOUND_MAX 4
static th_bound bound[TH_BOUND_MAX];
static unsigned bound_count;

static struct th_file input_stream  = { .kind = TH_FILE_EMPTY, .fd = 0, .pushed = -1, .in_use = 1 };
static struct th_file output_stream = { .kind = TH_FILE_DIAG,  .fd = 1, .pushed = -1, .in_use = 1 };
static struct th_file error_stream  = { .kind = TH_FILE_DIAG,  .fd = 2, .pushed = -1, .in_use = 1 };

FILE *stdin = &input_stream;
FILE *stdout = &output_stream;
FILE *stderr = &error_stream;

#define TH_OPEN_MAX 8
#define TH_FIRST_FD 3
static struct th_file opened[TH_OPEN_MAX];

void th_files_init(const th_config *config)
{
    bound_count = 0;
    if (config->bulk_bytes != 0) {
        bound[0].name = TH_BULK_PATH;
        bound[0].data = (const uint8_t *)(uintptr_t)TH_BULK_VADDR;
        bound[0].length = config->bulk_bytes;
        bound_count = 1;
    }
}

int th_files_name(unsigned index, const char **name)
{
    if (index >= bound_count) { return -1; }
    *name = bound[index].name;
    return 0;
}

static const th_bound *find(const char *path)
{
    for (unsigned i = 0; i < bound_count; i++) {
        if (strcmp(bound[i].name, path) == 0) { return &bound[i]; }
    }
    return NULL;
}

static struct th_file *open_bound(const th_bound *file)
{
    for (unsigned i = 0; i < TH_OPEN_MAX; i++) {
        int expected = 0;
        if (__atomic_compare_exchange_n(&opened[i].in_use, &expected, 1, 0, __ATOMIC_ACQ_REL,
                                        __ATOMIC_RELAXED)) {
            struct th_file *s = &opened[i];
            s->kind = TH_FILE_BLOB;
            s->data = file->data;
            s->length = file->length;
            s->position = 0;
            s->fd = TH_FIRST_FD + (int)i;
            s->eof = 0;
            s->error = 0;
            s->pushed = -1;
            return s;
        }
    }
    errno = EMFILE;
    return NULL;
}

static struct th_file *by_fd(int fd)
{
    if (fd == 0) { return &input_stream; }
    if (fd == 1) { return &output_stream; }
    if (fd == 2) { return &error_stream; }
    if (fd >= TH_FIRST_FD && fd < TH_FIRST_FD + TH_OPEN_MAX && opened[fd - TH_FIRST_FD].in_use) {
        return &opened[fd - TH_FIRST_FD];
    }
    errno = EBADF;
    return NULL;
}

static int read_only_mode(const char *mode)
{
    return strpbrk(mode, "wa+") == NULL;
}

/* ---------------------------------------------------------------- streams */

FILE *fopen(const char *path, const char *mode)
{
    if (path == NULL || mode == NULL) { errno = EINVAL; return NULL; }
    if (!read_only_mode(mode)) { errno = EROFS; return NULL; }
    const th_bound *file = find(path);
    if (file == NULL) { errno = ENOENT; return NULL; }
    return open_bound(file);
}

FILE *fopen64(const char *path, const char *mode) __attribute__((alias("fopen")));

FILE *fdopen(int fd, const char *mode)
{
    struct th_file *s = by_fd(fd);
    if (s != NULL && s->kind == TH_FILE_BLOB && !read_only_mode(mode)) {
        errno = EROFS;
        return NULL;
    }
    return s;
}

FILE *freopen(const char *path, const char *mode, FILE *stream)
{
    (void)path;
    (void)mode;
    (void)stream;
    errno = ENOSYS;
    return NULL;
}

int fclose(FILE *stream)
{
    if (stream == NULL) { errno = EBADF; return EOF; }
    if (stream >= &opened[0] && stream < &opened[TH_OPEN_MAX]) {
        __atomic_store_n(&stream->in_use, 0, __ATOMIC_RELEASE);
    }
    return 0;
}

size_t fread(void *into, size_t size, size_t count, FILE *s)
{
    if (size == 0 || count == 0) { return 0; }
    if (s->kind == TH_FILE_EMPTY) { s->eof = 1; return 0; }
    if (s->kind != TH_FILE_BLOB) { s->error = 1; errno = EBADF; return 0; }
    uint64_t want = (uint64_t)size * (uint64_t)count;
    if (want / size != count) { s->error = 1; errno = EOVERFLOW; return 0; }
    uint8_t *out = into;
    uint64_t got = 0;
    if (s->pushed >= 0) {
        out[got++] = (uint8_t)s->pushed;
        s->pushed = -1;
    }
    uint64_t left = s->position < s->length ? s->length - s->position : 0;
    uint64_t take = want - got < left ? want - got : left;
    memcpy(out + got, s->data + s->position, (size_t)take);
    s->position += take;
    got += take;
    if (got < want) { s->eof = 1; }
    return (size_t)(got / size);
}

size_t fwrite(const void *data, size_t size, size_t count, FILE *stream)
{
    if (size == 0 || count == 0) { return 0; }
    size_t written = th_stream_write(stream, data, size * count);
    return written / size;
}

static int reposition(struct th_file *s, int64_t offset, int whence)
{
    if (s->kind != TH_FILE_BLOB) { errno = ESPIPE; return -1; }
    int64_t base = whence == SEEK_SET ? 0
                 : whence == SEEK_CUR ? (int64_t)s->position
                 : whence == SEEK_END ? (int64_t)s->length : -1;
    if (base < 0 && whence != SEEK_SET) { errno = EINVAL; return -1; }
    int64_t target = base + offset;
    if (target < 0) { errno = EINVAL; return -1; }
    s->position = (uint64_t)target;
    s->eof = 0;
    s->pushed = -1;
    return 0;
}

int fseek(FILE *stream, long offset, int whence) { return reposition(stream, offset, whence); }
int fseeko(FILE *stream, off_t offset, int whence) { return reposition(stream, offset, whence); }
int fseeko64(FILE *stream, off_t offset, int whence) __attribute__((alias("fseeko")));

long ftell(FILE *s)
{
    if (s->kind != TH_FILE_BLOB) { errno = ESPIPE; return -1; }
    return (long)s->position - (s->pushed >= 0 ? 1 : 0);
}
off_t ftello(FILE *s) { return (off_t)ftell(s); }
off_t ftello64(FILE *s) __attribute__((alias("ftello")));

int fgetpos(FILE *stream, fpos_t *position)
{
    long at = ftell(stream);
    if (at < 0) { return -1; }
    position->__pos = at;
    position->__state = 0;
    return 0;
}

int fsetpos(FILE *stream, const fpos_t *position)
{
    return reposition(stream, position->__pos, SEEK_SET);
}

void rewind(FILE *stream)
{
    reposition(stream, 0, SEEK_SET);
    stream->error = 0;
}

void clearerr(FILE *stream) { stream->eof = 0; stream->error = 0; }
int feof(FILE *stream) { return stream->eof; }
int ferror(FILE *stream) { return stream->error; }
int fileno(FILE *stream) { return stream->fd; }

/* ------------------------------------------------------------- mappings */

/* The region behind a descriptor, at the address it is already mapped at.
 *
 * Refused unless every part of the request is one this system can answer
 * exactly: no chosen address, no offset, read-only, a descriptor that names a
 * bound region, and a length inside it. `EACCES` for a protection the region
 * does not have, `ENODEV` for anything else, because "this descriptor cannot
 * be mapped" and "this mapping would need a write authority" are different
 * refusals and a caller may act differently on them.
 */
void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset)
{
    if (addr != NULL || offset != 0 || length == 0) { errno = EINVAL; return MAP_FAILED; }
    if ((flags & (MAP_ANONYMOUS | MAP_FIXED)) != 0) { errno = ENODEV; return MAP_FAILED; }
    if ((prot & (PROT_WRITE | PROT_EXEC)) != 0) { errno = EACCES; return MAP_FAILED; }
    if ((prot & PROT_READ) == 0) { errno = EACCES; return MAP_FAILED; }
    struct th_file *s = by_fd(fd);
    if (s == NULL) { return MAP_FAILED; }          /* `by_fd` set EBADF */
    if (s->kind != TH_FILE_BLOB || s->data == NULL) { errno = ENODEV; return MAP_FAILED; }
    if ((uint64_t)length > s->length) { errno = EINVAL; return MAP_FAILED; }
    return (void *)(uintptr_t)s->data;
}

/* Refused: the mapping belongs to the domain, not to the program.
 *
 * It was installed under a capability the program does not hold and charged to
 * a scope the program does not control, so there is nothing here that could
 * honestly succeed. Returning success without removing anything would tell a
 * caller that pages were released when they were not; a caller that unmaps as
 * an optimisation carries on, and one that needs it finds out.
 */
int munmap(void *addr, size_t length)
{
    (void)addr;
    (void)length;
    errno = EPERM;
    return -1;
}

/* Accepted and does nothing, which is the truth: the region is resident from
 * the moment it was mapped, so there is nothing to fault in and nothing this
 * program may evict. */
int posix_madvise(void *addr, size_t length, int advice)
{
    (void)addr;
    (void)length;
    (void)advice;
    return 0;
}

int madvise(void *addr, size_t length, int advice)
{
    return posix_madvise(addr, length, advice);
}

/* The region is resident and cannot be paged out, so it is already locked in
 * the only sense this system has. Nothing is claimed beyond that. */
int mlock(const void *addr, size_t length) { (void)addr; (void)length; return 0; }
int munlock(const void *addr, size_t length) { (void)addr; (void)length; return 0; }

/* Nothing here buffers, so there is nothing a buffer mode could change. */
int setvbuf(FILE *stream, char *buffer, int mode, size_t size)
{
    (void)stream;
    (void)buffer;
    (void)size;
    return mode == _IOFBF || mode == _IOLBF || mode == _IONBF ? 0 : -1;
}
void setbuf(FILE *stream, char *buffer) { (void)stream; (void)buffer; }
int fflush(FILE *stream) { (void)stream; return 0; }

int fgetc(FILE *s)
{
    if (s->pushed >= 0) {
        int c = s->pushed;
        s->pushed = -1;
        return c;
    }
    if (s->kind == TH_FILE_BLOB && s->position < s->length) {
        return s->data[s->position++];
    }
    if (s->kind != TH_FILE_BLOB && s->kind != TH_FILE_EMPTY) {
        s->error = 1;
        errno = EBADF;
        return EOF;
    }
    s->eof = 1;
    return EOF;
}

int getc(FILE *stream) { return fgetc(stream); }
int getchar(void) { return fgetc(stdin); }

int ungetc(int c, FILE *stream)
{
    if (c == EOF || stream->pushed >= 0) { return EOF; }
    stream->pushed = (unsigned char)c;
    stream->eof = 0;
    return (unsigned char)c;
}

char *fgets(char *out, int n, FILE *stream)
{
    if (n <= 0) { return NULL; }
    int at = 0;
    while (at + 1 < n) {
        int c = fgetc(stream);
        if (c == EOF) { break; }
        out[at++] = (char)c;
        if (c == '\n') { break; }
    }
    if (at == 0) { return NULL; }
    out[at] = 0;
    return out;
}

void perror(const char *prefix)
{
    if (prefix && *prefix) { fprintf(stderr, "%s: %s\n", prefix, strerror(errno)); }
    else { fprintf(stderr, "%s\n", strerror(errno)); }
}

int remove(const char *path) { (void)path; errno = EROFS; return -1; }
int rename(const char *from, const char *to) { (void)from; (void)to; errno = EROFS; return -1; }
FILE *tmpfile(void) { errno = EROFS; return NULL; }
char *tmpnam(char *out) { (void)out; return NULL; }

/* ------------------------------------------------------------ descriptors */

int open(const char *path, int flags, ...)
{
    if ((flags & O_ACCMODE) != O_RDONLY || (flags & (O_CREAT | O_TRUNC | O_APPEND))) {
        errno = EROFS;
        return -1;
    }
    const th_bound *file = find(path);
    if (file == NULL) { errno = ENOENT; return -1; }
    struct th_file *s = open_bound(file);
    return s ? s->fd : -1;
}

int open64(const char *path, int flags, ...) __attribute__((alias("open")));

ssize_t read(int fd, void *into, size_t n)
{
    struct th_file *s = by_fd(fd);
    if (s == NULL) { return -1; }
    if (s->kind == TH_FILE_DIAG) { errno = EBADF; return -1; }
    size_t got = fread(into, 1, n, s);
    return s->error ? -1 : (ssize_t)got;
}

ssize_t write(int fd, const void *from, size_t n)
{
    struct th_file *s = by_fd(fd);
    if (s == NULL) { return -1; }
    if (s->kind != TH_FILE_DIAG) { errno = EBADF; return -1; }
    return (ssize_t)th_stream_write(s, from, n);
}

int close(int fd)
{
    struct th_file *s = by_fd(fd);
    if (s == NULL) { return -1; }
    return fclose(s);
}

off_t lseek(int fd, off_t offset, int whence)
{
    struct th_file *s = by_fd(fd);
    if (s == NULL) { return -1; }
    if (reposition(s, offset, whence) != 0) { return -1; }
    return (off_t)s->position;
}
off_t lseek64(int fd, off_t offset, int whence) __attribute__((alias("lseek")));

static void describe(struct stat *out, const struct th_file *s, uint64_t length, int regular)
{
    memset(out, 0, sizeof(*out));
    out->st_nlink = 1;
    out->st_mode = regular ? (S_IFREG | 0444) : (S_IFCHR | 0600);
    out->st_size = (off_t)length;
    out->st_blksize = 4096;
    out->st_blocks = (blkcnt_t)((length + 511) / 512);
    out->st_ino = s ? (ino_t)(s->fd + 1) : 1;
}

int fstat(int fd, struct stat *out)
{
    struct th_file *s = by_fd(fd);
    if (s == NULL) { return -1; }
    describe(out, s, s->kind == TH_FILE_BLOB ? s->length : 0, s->kind == TH_FILE_BLOB);
    return 0;
}
int fstat64(int fd, struct stat *out) __attribute__((alias("fstat")));

int stat(const char *path, struct stat *out)
{
    const th_bound *file = find(path);
    if (file == NULL) { errno = ENOENT; return -1; }
    describe(out, NULL, file->length, 1);
    return 0;
}
int stat64(const char *path, struct stat *out) __attribute__((alias("stat")));
int lstat(const char *path, struct stat *out) { return stat(path, out); }

int isatty(int fd) { (void)fd; errno = ENOTTY; return 0; }

/* No descriptor here is a terminal or a device. */
int ioctl(int fd, unsigned long request, ...)
{
    (void)request;
    if (by_fd(fd) == NULL) { return -1; }
    errno = ENOTTY;
    return -1;
}

/* Two names for one stream would be two positions in one file; nothing here
 * needs that and nothing pretends to provide it. */
int dup(int fd) { (void)fd; errno = ENOSYS; return -1; }
int dup2(int fd, int to) { (void)fd; (void)to; errno = ENOSYS; return -1; }

int fcntl(int fd, int command, ...)
{
    struct th_file *s = by_fd(fd);
    if (s == NULL) { return -1; }
    if (command == F_GETFL) { return s->kind == TH_FILE_DIAG ? O_WRONLY : O_RDONLY; }
    if (command == F_GETFD || command == F_SETFD) { return 0; }
    errno = EINVAL;
    return -1;
}

int access(const char *path, int mode)
{
    if (find(path) == NULL) { errno = ENOENT; return -1; }
    if (mode & (W_OK | X_OK)) { errno = EACCES; return -1; }
    return 0;
}

/* One directory, holding what was bound. */
char *getcwd(char *into, size_t n)
{
    if (into == NULL || n < 2) { errno = ERANGE; return NULL; }
    into[0] = '/';
    into[1] = 0;
    return into;
}

int unlink(const char *path) { (void)path; errno = EROFS; return -1; }
int mkdir(const char *path, mode_t mode) { (void)path; (void)mode; errno = EROFS; return -1; }
