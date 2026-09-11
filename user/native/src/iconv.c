/* Character set conversion, refused. See `iconv.h`. */

#include <iconv.h>
#include <errno.h>

iconv_t iconv_open(const char *to, const char *from)
{
    (void)to;
    (void)from;
    errno = EINVAL;
    return (iconv_t)-1;
}

size_t iconv(iconv_t cd, char **in, size_t *in_left, char **out, size_t *out_left)
{
    (void)cd;
    (void)in;
    (void)in_left;
    (void)out;
    (void)out_left;
    errno = EBADF;
    return (size_t)-1;
}

int iconv_close(iconv_t cd)
{
    (void)cd;
    return 0;
}
