/* Directories: there is one, and it lists what was bound.
 *
 * The prebuilt C++ library's `std::filesystem` walks directories through this
 * interface and reads the entries by glibc's x86-64 layout, so the structure is
 * that one. The only directory is `/`, and its entries are the read-only files
 * the domain was given (see `stdio.h`). There are no directory descriptors, so
 * `fdopendir` refuses.
 */
#ifndef _DIRENT_H
#define _DIRENT_H

#include <sys/types.h>
#include "thalyx/cdefs.h"

struct dirent {
    ino_t d_ino;
    off_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[256];
};

#define DT_UNKNOWN 0
#define DT_DIR     4
#define DT_REG     8

typedef struct th_dir DIR;

__TH_BEGIN_DECLS
DIR *opendir(const char *path);
DIR *fdopendir(int fd);
struct dirent *readdir(DIR *dir);
int closedir(DIR *dir);
void rewinddir(DIR *dir);
int dirfd(DIR *dir) __TH_NOTHROW;
__TH_END_DECLS

#endif
