/* The one question a prebuilt C++ library asks of this C library's version.
 *
 * libstdc++'s configuration headers include <features.h> for `__GLIBC_PREREQ`
 * and nothing else. The archive this target links was built against glibc
 * 2.44, and the answers its headers get have to be the ones it was compiled
 * with: the layouts inside the archive are fixed, and a header that answered
 * differently would describe objects the archive does not contain.
 *
 * `__GLIBC__` itself is deliberately left undefined. This library is not glibc;
 * it implements the part of glibc's x86-64 interface that the prebuilt archive
 * links against, and code that keys a glibc-only path on `__GLIBC__` must not
 * take that path here.
 */
#ifndef _FEATURES_H
#define _FEATURES_H

#define __GLIBC_PREREQ(major, minor) ((major) < 2 || ((major) == 2 && (minor) <= 44))

#define __THALYX_NATIVE__ 1

#endif
