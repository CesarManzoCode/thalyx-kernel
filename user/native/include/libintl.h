/* Message catalogues: there are none.
 *
 * The C++ library's `std::messages` facet is written over this interface, and
 * `<locale>` includes it whether or not a program asks for a message. Every
 * lookup answers the text it was given, which is what a catalogue that has no
 * translation for it would answer too.
 */
#ifndef _LIBINTL_H
#define _LIBINTL_H

#include "thalyx/cdefs.h"

__TH_BEGIN_DECLS
char *gettext(const char *msgid) __TH_NOTHROW;
char *dgettext(const char *domain, const char *msgid) __TH_NOTHROW;
char *dcgettext(const char *domain, const char *msgid, int category) __TH_NOTHROW;
char *textdomain(const char *domain) __TH_NOTHROW;
char *bindtextdomain(const char *domain, const char *dir) __TH_NOTHROW;
char *bind_textdomain_codeset(const char *domain, const char *codeset) __TH_NOTHROW;
__TH_END_DECLS

#endif
