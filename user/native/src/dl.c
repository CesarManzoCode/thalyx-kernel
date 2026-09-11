/* Dynamic loading, refused. See `dlfcn.h`. */

#include <dlfcn.h>
#include <stddef.h>

static const char *last_error;

void *dlopen(const char *path, int mode)
{
    (void)path;
    (void)mode;
    last_error = "dynamic loading is not supported: a native image is linked whole";
    return NULL;
}

void *dlsym(void *handle, const char *name)
{
    (void)handle;
    (void)name;
    last_error = "no object is loaded to look a symbol up in";
    return NULL;
}

int dlclose(void *handle)
{
    (void)handle;
    last_error = "no object is loaded to close";
    return -1;
}

char *dlerror(void)
{
    const char *error = last_error;
    last_error = NULL;
    return (char *)error;
}
