/* Device control: there are no terminals.
 *
 * Declared because ported code asks a terminal how wide it is on every POSIX
 * platform. Nothing here is a terminal, so every request is `ENOTTY`, which is
 * the answer the same code gets on a POSIX system when its output is a pipe.
 */
#ifndef _SYS_IOCTL_H
#define _SYS_IOCTL_H

#include "thalyx/cdefs.h"

struct winsize {
    unsigned short ws_row;
    unsigned short ws_col;
    unsigned short ws_xpixel;
    unsigned short ws_ypixel;
};

#define TIOCGWINSZ 0x5413
#define FIONREAD   0x541B

__TH_BEGIN_DECLS
int ioctl(int fd, unsigned long request, ...) __TH_NOTHROW;
__TH_END_DECLS

#endif
