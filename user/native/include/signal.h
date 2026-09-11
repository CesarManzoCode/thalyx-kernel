/* Signals: there are none.
 *
 * Nothing on this kernel delivers an asynchronous signal to a domain. A fault
 * is a message on the domain's fault channel, handled by its supervisor, and
 * a domain that faults stops. So `signal` refuses with `ENOSYS` rather than
 * installing a handler nothing would ever call, and `raise` of anything but
 * `SIGABRT` is refused the same way; `SIGABRT` is `abort`, which is what
 * raising it means.
 */
#ifndef _SIGNAL_H
#define _SIGNAL_H

#include "thalyx/cdefs.h"

typedef int sig_atomic_t;
typedef void (*sighandler_t)(int);

#define SIGHUP   1
#define SIGINT   2
#define SIGQUIT  3
#define SIGILL   4
#define SIGTRAP  5
#define SIGABRT  6
#define SIGBUS   7
#define SIGFPE   8
#define SIGKILL  9
#define SIGSEGV 11
#define SIGPIPE 13
#define SIGALRM 14
#define SIGTERM 15

#define SIG_DFL ((sighandler_t) 0)
#define SIG_IGN ((sighandler_t) 1)
#define SIG_ERR ((sighandler_t) -1)

__TH_BEGIN_DECLS
sighandler_t signal(int sig, sighandler_t handler) __TH_NOTHROW;
int raise(int sig) __TH_NOTHROW;
__TH_END_DECLS

#endif
