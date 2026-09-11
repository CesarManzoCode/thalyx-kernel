/* Character classes of the C locale, in glibc's table layout.
 *
 * The tables are indexed from -128 to 255, which is how glibc lays them out
 * and how the prebuilt C++ library indexes them through `__ctype_b_loc()`. A
 * byte above 0x7F has no class in the C locale; neither does EOF.
 */

#include <ctype.h>
#include <stdint.h>

#define PUNCT  (_ISpunct | _ISprint | _ISgraph)
#define DIGIT  (_ISdigit | _ISxdigit | _ISalnum | _ISprint | _ISgraph)
#define UPPERX (_ISupper | _ISalpha | _ISxdigit | _ISalnum | _ISprint | _ISgraph)
#define UPPER  (_ISupper | _ISalpha | _ISalnum | _ISprint | _ISgraph)
#define LOWERX (_ISlower | _ISalpha | _ISxdigit | _ISalnum | _ISprint | _ISgraph)
#define LOWER  (_ISlower | _ISalpha | _ISalnum | _ISprint | _ISgraph)

static const unsigned short classes[384] = {
    [128 + 0x00 ... 128 + 0x08] = _IScntrl,
    [128 + '\t'] = _IScntrl | _ISspace | _ISblank,
    [128 + '\n' ... 128 + '\r'] = _IScntrl | _ISspace,
    [128 + 0x0E ... 128 + 0x1F] = _IScntrl,
    [128 + ' '] = _ISspace | _ISblank | _ISprint,
    [128 + '!' ... 128 + '/'] = PUNCT,
    [128 + '0' ... 128 + '9'] = DIGIT,
    [128 + ':' ... 128 + '@'] = PUNCT,
    [128 + 'A' ... 128 + 'F'] = UPPERX,
    [128 + 'G' ... 128 + 'Z'] = UPPER,
    [128 + '[' ... 128 + '`'] = PUNCT,
    [128 + 'a' ... 128 + 'f'] = LOWERX,
    [128 + 'g' ... 128 + 'z'] = LOWER,
    [128 + '{' ... 128 + '~'] = PUNCT,
    [128 + 0x7F] = _IScntrl,
};

static const unsigned short *classes_at = classes + 128;

static int32_t lower_table[384];
static int32_t upper_table[384];
static const int32_t *lower_at = lower_table + 128;
static const int32_t *upper_at = upper_table + 128;
static int tables_ready;

/* Built on first use. Two threads that race here write the same values. */
static void build_case_tables(void)
{
    for (int c = -128; c < 256; c++) {
        lower_table[c + 128] = (c >= 'A' && c <= 'Z') ? c + 32 : c;
        upper_table[c + 128] = (c >= 'a' && c <= 'z') ? c - 32 : c;
    }
    __atomic_store_n(&tables_ready, 1, __ATOMIC_RELEASE);
}

const unsigned short **__ctype_b_loc(void) { return &classes_at; }

const int32_t **__ctype_tolower_loc(void)
{
    if (!__atomic_load_n(&tables_ready, __ATOMIC_ACQUIRE)) { build_case_tables(); }
    return &lower_at;
}

const int32_t **__ctype_toupper_loc(void)
{
    if (!__atomic_load_n(&tables_ready, __ATOMIC_ACQUIRE)) { build_case_tables(); }
    return &upper_at;
}

static int has(int c, unsigned short mask)
{
    return (c >= -128 && c < 256) ? (classes[c + 128] & mask) != 0 : 0;
}

int isalnum(int c)  { return has(c, _ISalnum); }
int isalpha(int c)  { return has(c, _ISalpha); }
int isblank(int c)  { return has(c, _ISblank); }
int iscntrl(int c)  { return has(c, _IScntrl); }
int isdigit(int c)  { return has(c, _ISdigit); }
int isgraph(int c)  { return has(c, _ISgraph); }
int islower(int c)  { return has(c, _ISlower); }
int isprint(int c)  { return has(c, _ISprint); }
int ispunct(int c)  { return has(c, _ISpunct); }
int isspace(int c)  { return has(c, _ISspace); }
int isupper(int c)  { return has(c, _ISupper); }
int isxdigit(int c) { return has(c, _ISxdigit); }
int isascii(int c)  { return (c & ~0x7F) == 0; }
int tolower(int c)  { return (c >= 'A' && c <= 'Z') ? c + 32 : c; }
int toupper(int c)  { return (c >= 'a' && c <= 'z') ? c - 32 : c; }
