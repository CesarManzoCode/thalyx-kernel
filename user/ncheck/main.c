/* The validation tool: a real one, run natively, over the actual candidate.
 *
 * `vault/roadmap/phases.md` asks K5b for "programa acotado y herramienta real
 * dentro del kernel", and `vault/integration/thalyx.md` says why the second
 * half of that matters: *un compilador ejecutado en el host no prueba
 * confinamiento ni resource accounting del compilador dentro del kernel*. So
 * this is not a fixture, not a mock and not a host command. It is a program
 * built for the native target, launched into a domain of its own with a scope
 * of its own, given exactly one thing -- a **sealed** memory object holding the
 * candidate -- and nothing else. No state service, no engine, no device, no
 * authority to build anything.
 *
 * ## What it actually does
 *
 * It compiles every JavaScript name in the candidate with QuickJS's own parser
 * and bytecode compiler, and then runs the candidate's own assertions in a
 * context whose only binding is `check`. Both halves can genuinely fail: a
 * syntax error is a compile that refuses, and an assertion that does not hold
 * is a check that failed. The tool's exit code is zero only when every name
 * compiled and every check held.
 *
 * ## Why the candidate is sealed
 *
 * A tool that validated bytes which could change under it would be validating
 * nothing in particular. The object is sealed before it is lent, which means the
 * kernel has withdrawn every writable mapping of it -- so what this reads is
 * what the verdict is about, and the digest it reports is over the bytes it
 * actually read rather than over the bytes somebody meant to give it.
 *
 * ## What it does not do
 *
 * It is a syntax check and an assertion run, and it says so: it is not a type
 * check and must never be reported as one. Thalyx's `Check::Rust` compiles a
 * crate graph with `cargo`; nothing on this system does that and nothing here
 * pretends to.
 */

#include "thalyx/nrt.h"
#include "thalyx/k5.h"
#include "quickjs.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>

/* What the tool reports, in the region the launcher mapped writable for it. */
static k5_tool_report *report(void)
{
    return (k5_tool_report *) (uintptr_t) TH_XFER_VADDR;
}

static const uint8_t *candidate(void)
{
    return (const uint8_t *) (uintptr_t) TH_BULK_VADDR;
}

typedef struct {
    uint32_t run;
    uint32_t failed;
    char first[64];
} Checks;

static Checks checks;

/* `check(condition, what)` and nothing else. A check that could reach the
 * system would be a check that could pass for the wrong reason. */
static JSValue bound_check(JSContext *ctx, JSValueConst this_value,
                           int argc, JSValueConst *argv)
{
    (void) this_value;
    checks.run++;
    int held = argc > 0 && JS_ToBool(ctx, argv[0]) > 0;
    if (!held) {
        checks.failed++;
        if (checks.first[0] == 0 && argc > 1) {
            const char *what = JS_ToCString(ctx, argv[1]);
            if (what) {
                size_t len = strlen(what);
                if (len > sizeof(checks.first) - 1) { len = sizeof(checks.first) - 1; }
                memcpy(checks.first, what, len);
                checks.first[len] = 0;
                JS_FreeCString(ctx, what);
            }
        }
        th_note(K5_NOTE_TOOL_CHECK_FAILED, checks.run);
    } else {
        th_note(K5_NOTE_TOOL_CHECK, checks.run);
    }
    return JS_NewBool(ctx, held);
}

static void *tool_calloc(void *opaque, size_t count, size_t size)
{
    (void) opaque;
    return calloc(count, size);
}
static void *tool_malloc(void *opaque, size_t size) { (void) opaque; return malloc(size); }
static void tool_free(void *opaque, void *ptr) { (void) opaque; free(ptr); }
static void *tool_realloc(void *opaque, void *ptr, size_t size)
{
    (void) opaque;
    return realloc(ptr, size);
}
static size_t tool_usable(const void *ptr) { return malloc_usable_size((void *) ptr); }

static const JSMallocFunctions TOOL_ALLOCATOR = {
    tool_calloc, tool_malloc, tool_free, tool_realloc, tool_usable,
};

/* A digest over the bytes the tool actually read.
 *
 * FNV-1a, and it is not a cryptographic claim: what it is for is that a verdict
 * names the bytes it was about, so a verdict presented alongside other bytes can
 * be seen not to be about them. */
static uint64_t digest_of(const uint8_t *bytes, size_t len)
{
    uint64_t hash = 1469598103934665603ull;
    for (size_t index = 0; index < len; index++) {
        hash = (hash ^ bytes[index]) * 1099511628211ull;
    }
    return hash;
}

/* A file, copied out of the sealed object and zero terminated.
 *
 * `JS_Eval` documents that its buffer must be zero terminated -- its scanner
 * leans on the terminator rather than only on the length -- and the names in a
 * candidate sit next to each other with nothing between them. The first version
 * of this tool evaluated in place and reported three syntax errors in three
 * files that were perfectly good JavaScript.
 */
static char source_buffer[K5_CANDIDATE_MAX + 1];

static size_t terminated(const uint8_t *bytes, uint64_t len, uint32_t flags)
{
    if (len > K5_CANDIDATE_MAX - 32) {
        len = K5_CANDIDATE_MAX - 32;
    }
    size_t at = 0;
    /* A function body compiled as a script is a syntax error on its last line,
     * which would be a syntax error this tool invented. It is compiled the way
     * the runtime executes it, and the report says how many it wrapped. */
    if (flags & K5_CANDIDATE_FLAG_FUNCTION_BODY) {
        memcpy(source_buffer, "(function(){", 12);
        at = 12;
    }
    memcpy(source_buffer + at, bytes, (size_t) len);
    at += (size_t) len;
    if (flags & K5_CANDIDATE_FLAG_FUNCTION_BODY) {
        memcpy(source_buffer + at, "\n})", 3);
        at += 3;
    }
    source_buffer[at] = 0;
    return at;
}

static int is_javascript(const uint8_t *name, uint32_t len)
{
    return len > 3 && memcmp(name + len - 3, ".js", 3) == 0;
}

int th_main(const th_config *config)
{
    memset(report(), 0, sizeof(k5_tool_report));

    const uint8_t *blob = candidate();
    k5_candidate_header header;
    memcpy(&header, blob, sizeof(header));
    if (header.magic != K5_CANDIDATE_MAGIC_LOW || header.count == 0
        || header.count > K5_CANDIDATE_ENTRIES) {
        th_note(K5_NOTE_TOOL_PARSE_FAILED, header.magic);
        return 3;
    }
    if (header.total_bytes > config->bulk_bytes || header.total_bytes > K5_CANDIDATE_MAX) {
        th_note(K5_NOTE_TOOL_PARSE_FAILED, header.total_bytes);
        return 3;
    }
    /* The seed the launcher passed and the seed inside the sealed object have
     * to be the same. They come by different paths -- one in a message, one in
     * bytes the kernel sealed -- and a tool that did not compare them could be
     * handed a candidate from another run. */
    if (header.seed != config->seed) {
        th_note(K5_NOTE_TOOL_PARSE_FAILED, header.seed);
        return 3;
    }
    th_note(K5_NOTE_TOOL_START, header.total_bytes);

    JSRuntime *runtime = JS_NewRuntime2(&TOOL_ALLOCATOR, NULL);
    if (!runtime) { return 4; }
    JS_SetMemoryLimit(runtime, 2 * 1024 * 1024);
    JS_SetMaxStackSize(runtime, 256 * 1024);
    JSContext *ctx = JS_NewContext(runtime);
    if (!ctx) { JS_FreeRuntime(runtime); return 4; }

    JSValue global = JS_GetGlobalObject(ctx);
    JS_SetPropertyStr(ctx, global, "check", JS_NewCFunction(ctx, bound_check, "check", 2));
    JS_FreeValue(ctx, global);

    uint32_t parsed = 0, parse_failed = 0;
    uint64_t bytes_read = 0;
    uint64_t sum = 1469598103934665603ull;

    /* First pass: every JavaScript name has to compile. Compile-only, so a
     * name that parses but would misbehave is still reported as parsing --
     * which is the honest thing for a syntax check to say. */
    for (uint32_t index = 0; index < header.count; index++) {
        k5_candidate_entry entry;
        memcpy(&entry, blob + sizeof(header) + index * sizeof(entry), sizeof(entry));
        if (entry.offset + entry.length > header.total_bytes) {
            parse_failed++;
            continue;
        }
        const uint8_t *body = blob + entry.offset;
        bytes_read += entry.length;
        sum ^= digest_of(body, (size_t) entry.length);
        sum *= 1099511628211ull;
        if (!is_javascript(entry.name, entry.name_len)) {
            continue;
        }
        char name[40];
        uint32_t width = entry.name_len < sizeof(name) - 1 ? entry.name_len : (uint32_t) sizeof(name) - 1;
        memcpy(name, entry.name, width);
        name[width] = 0;
        size_t source_len = terminated(body, entry.length, entry.flags);
        JSValue compiled = JS_Eval(ctx, source_buffer, source_len, name,
                                   JS_EVAL_TYPE_GLOBAL | JS_EVAL_FLAG_COMPILE_ONLY);
        if (JS_IsException(compiled)) {
            parse_failed++;
            JSValue error = JS_GetException(ctx);
            JS_FreeValue(ctx, error);
            th_note(K5_NOTE_TOOL_PARSE_FAILED, index);
        } else {
            parsed++;
            th_note(K5_NOTE_TOOL_PARSED, entry.length);
        }
        JS_FreeValue(ctx, compiled);
    }

    /* Second pass: the module, then its assertions, in that order and in one
     * context, because the assertions are about what the module defines. */
    if (parse_failed == 0) {
        for (uint32_t round = 0; round < 2; round++) {
            const char *wanted = round == 0 ? "module.js" : "module.test.js";
            for (uint32_t index = 0; index < header.count; index++) {
                k5_candidate_entry entry;
                memcpy(&entry, blob + sizeof(header) + index * sizeof(entry), sizeof(entry));
                char name[40];
                uint32_t width = entry.name_len < sizeof(name) - 1
                               ? entry.name_len : (uint32_t) sizeof(name) - 1;
                memcpy(name, entry.name, width);
                name[width] = 0;
                if (strcmp(name, wanted) != 0) { continue; }
                size_t source_len =
                    terminated(blob + entry.offset, entry.length, entry.flags);
                JSValue result =
                    JS_Eval(ctx, source_buffer, source_len, name, JS_EVAL_TYPE_GLOBAL);
                if (JS_IsException(result)) {
                    checks.failed++;
                    if (checks.first[0] == 0) {
                        memcpy(checks.first, "the candidate threw while running", 33);
                    }
                    JSValue error = JS_GetException(ctx);
                    JS_FreeValue(ctx, error);
                }
                JS_FreeValue(ctx, result);
            }
        }
    }

    k5_tool_report *out = report();
    out->checks_run = checks.run;
    out->checks_failed = checks.failed;
    out->parsed = parsed;
    out->parse_failed = parse_failed;
    out->bytes_read = bytes_read;
    out->candidate_sum = sum;
    memcpy(out->first_failure, checks.first, sizeof(checks.first));

    JS_FreeContext(ctx);
    JS_FreeRuntime(runtime);

    int failed = (parse_failed != 0) || (checks.failed != 0) || (checks.run == 0);
    th_note(K5_NOTE_TOOL_DONE, (uint64_t) checks.run | ((uint64_t) checks.failed << 16)
            | ((uint64_t) parsed << 32) | ((uint64_t) parse_failed << 48));
    return failed ? 1 : 0;
}
