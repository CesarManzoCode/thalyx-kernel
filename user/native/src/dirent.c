/* The one directory. See `dirent.h`. */

#include "thalyx/nrt.h"
#include <dirent.h>
#include <string.h>
#include <errno.h>
#include <stdio.h>
#include <sys/stat.h>

struct th_dir {
    int in_use;
    unsigned next;
    struct dirent entry;
};

static struct th_dir directories[2];

/* The names bound in `file.c`, asked for one at a time. */
int th_files_name(unsigned index, const char **name);

DIR *opendir(const char *path)
{
    if (path == NULL || strcmp(path, "/") != 0) {
        struct stat st;
        errno = (path && stat(path, &st) == 0) ? ENOTDIR : ENOENT;
        return NULL;
    }
    for (unsigned i = 0; i < sizeof(directories) / sizeof(directories[0]); i++) {
        int expected = 0;
        if (__atomic_compare_exchange_n(&directories[i].in_use, &expected, 1, 0,
                                        __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) {
            directories[i].next = 0;
            return &directories[i];
        }
    }
    errno = EMFILE;
    return NULL;
}

DIR *fdopendir(int fd)
{
    (void)fd;
    errno = ENOTDIR;
    return NULL;
}

struct dirent *readdir(DIR *dir)
{
    const char *name;
    if (th_files_name(dir->next, &name) != 0) { return NULL; }
    const char *base = strrchr(name, '/');
    base = base ? base + 1 : name;
    memset(&dir->entry, 0, sizeof(dir->entry));
    dir->entry.d_ino = dir->next + 1;
    dir->entry.d_off = dir->next + 1;
    dir->entry.d_reclen = sizeof(dir->entry);
    dir->entry.d_type = DT_REG;
    strncpy(dir->entry.d_name, base, sizeof(dir->entry.d_name) - 1);
    dir->next++;
    return &dir->entry;
}

int closedir(DIR *dir)
{
    __atomic_store_n(&dir->in_use, 0, __ATOMIC_RELEASE);
    return 0;
}

void rewinddir(DIR *dir) { dir->next = 0; }

int dirfd(DIR *dir)
{
    (void)dir;
    errno = ENOTSUP;
    return -1;
}
