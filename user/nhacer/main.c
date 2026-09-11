/* `hacer`: a short program from a model, executed here.
 *
 * This is the language runtime of the port, and it is real QuickJS -- the same
 * engine, at the same version, that the Thalyx revision this port is against
 * runs: `rquickjs 0.12` vendors quickjs-ng 0.15.1, and that is what
 * `tools/fetch_quickjs.py` fetches and `tools/build_native.py` compiles for the
 * native target. Nothing about the language is reimplemented here.
 *
 * ## Why the engine and not an interpreter of our own
 *
 * The property that makes a language runtime the right thing to give an
 * untrusted program is that it *starts with no authority*. QuickJS's core is a
 * language and nothing else: no filesystem, no network, no process, no clock
 * that reaches the outside, no `require`. A program starts able to do
 * arithmetic, and the only things it can reach are the functions bound below.
 * That is the opposite of embedding a shell, where the work would be
 * subtracting authority from something that starts with all of it.
 *
 * ## What this domain holds
 *
 * A facet of one endpoint, a page of configuration, a region shared with the
 * work that drives it, and its own scope so it can grow a heap. That is the
 * whole of its capability table, and it is why "the program cannot read a file"
 * is a fact about the kernel rather than a claim about this source: there is no
 * file to read and no capability that would reach one.
 *
 * ## Two ceilings, and only one of them is the engine's
 *
 * `JS_SetMemoryLimit` is the engine's own accounting and it is set *below* what
 * the work's scope can charge. The scope's ceiling is the kernel's, enforced
 * against a program that found a way past the first. A run that hits either one
 * says which, in the units it is counted in, because "it ran out of memory" is
 * two different diagnoses.
 *
 * ## Stopping is enforced twice
 *
 * An assertion that only threw could be caught by the program that failed it,
 * and the run would carry on past the thing that was supposed to end it. So a
 * failed assertion **latches**: it is recorded on this side, it throws, and from
 * that moment the interrupt handler stops the engine and every host call
 * refuses. A program cannot catch its way past a stop, because the thing that
 * stops it is not in the language.
 */

#include "thalyx/nrt.h"
#include "thalyx/k5.h"
#include "quickjs.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>

/* The prelude, turned into a C string by the build. */
#include "prelude.inc"

/* ------------------------------------------------------------- the machine */

/* What the runtime holds while a program runs. Nothing here is reachable from
 * the program: it is on this side of the door. */
typedef struct {
    uint64_t facet;          /* the work this runtime answers to              */
    uint8_t *channel;        /* the region shared with it                     */
    uint64_t calls;          /* host calls made                               */
    uint64_t requests;
    uint64_t validations;
    uint64_t observations;
    uint64_t assertions;
    uint64_t inferences;
    uint64_t answer_bytes;
    uint64_t ticks;
    uint64_t started_ns;
    uint64_t wall_ns;        /* the ceiling on how long the program may run   */
    uint64_t tick_ceiling;
    uint64_t call_ceiling;
    int      latched;        /* an assertion said no; everything after refuses */
    int      needs_model;
    uint32_t finish;
    uint64_t returned_bytes;
} Machine;

static Machine machine;

/* --------------------------------------------------------------- the door */

/* One host call: the bytes go in the shared region, the inline message says how
 * many, and the answer comes back the same way.
 *
 * The region is mapped writable in both domains, which is the cheap way to move
 * more than a message can carry and is exactly why the work copies a request
 * into its own memory before it looks at it. Nothing here may assume the work
 * trusts these bytes; nothing there may assume they stop changing.
 */
static int host_call(uint32_t op, const uint8_t *request, uint32_t request_len,
                     uint64_t arg0, uint64_t arg1,
                     uint8_t **answer, uint32_t *answer_len, uint64_t *value)
{
    if (request_len > K5_CHANNEL_REQUEST_MAX) {
        return -1;
    }
    if (request_len > 0) {
        memcpy(machine.channel + K5_CHANNEL_REQUEST_OFFSET, request, request_len);
    }

    k5_host_request framed;
    memset(&framed, 0, sizeof(framed));
    framed.op = op;
    framed.request_len = request_len;
    framed.cookie = ++machine.calls;
    framed.arg0 = arg0;
    framed.arg1 = arg1;

    th_payload out;
    memcpy(out.bytes, &framed, sizeof(framed));
    out.len = (uint32_t) sizeof(framed);

    th_payload in;
    in.len = 0;
    int64_t status = th_call(machine.facet, &out, &in, 0, 0, 0);
    if (status != 0 || in.len < sizeof(k5_host_reply)) {
        return -1;
    }
    k5_host_reply reply;
    memcpy(&reply, in.bytes, sizeof(reply));
    if (reply.stopped) {
        machine.latched = 1;
    }
    if (answer) {
        *answer = machine.channel + K5_CHANNEL_ANSWER_OFFSET;
    }
    if (answer_len) {
        *answer_len = reply.answer_len;
    }
    if (value) {
        *value = reply.value;
    }
    machine.answer_bytes += reply.answer_len;
    return reply.status == 0 ? 0 : -1;
}

/* An answer, parsed by the engine's own JSON reader.
 *
 * Not by a reader written here: the work writes JSON and the engine reads it,
 * so neither side has an opinion about a format the other invented. A refusal
 * comes back as a value the program branches on.
 *
 * The bytes are copied out of the shared region first, and that is not
 * tidiness. Two reasons, and both were paid for:
 *
 *  - `JS_ParseJSON` documents that its buffer must be zero terminated -- its
 *    scanner leans on the terminator rather than only on the length -- and a
 *    region shared with another domain has whatever the last answer left in it
 *    at that offset. The first version of this file parsed in place and the
 *    third call of the first real program came back "unexpected data at the
 *    end", with a hundred and forty-two bytes of perfectly good JSON in front
 *    of it.
 *  - The region is writable in the other domain, so parsing in place would be
 *    reading bytes that can change under the parser. This side copies before it
 *    looks, exactly as the work does in the other direction.
 */
static uint8_t parse_buffer[K5_CHANNEL_ANSWER_MAX + 1];

static JSValue answer_value(JSContext *ctx, const uint8_t *bytes, uint32_t len)
{
    if (len == 0) {
        return JS_NULL;
    }
    if (len > K5_CHANNEL_ANSWER_MAX) {
        len = K5_CHANNEL_ANSWER_MAX;
    }
    memcpy(parse_buffer, bytes, len);
    parse_buffer[len] = 0;
    return JS_ParseJSON(ctx, (const char *) parse_buffer, len, "<answer>");
}

static JSValue refusal(JSContext *ctx, const char *word)
{
    JSValue object = JS_NewObject(ctx);
    JS_SetPropertyStr(ctx, object, "ok", JS_FALSE);
    JS_SetPropertyStr(ctx, object, "error", JS_NewString(ctx, word));
    return object;
}

/* ------------------------------------------------------------- the binding */

static JSValue bound_call(JSContext *ctx, JSValueConst this_value,
                          int argc, JSValueConst *argv)
{
    (void) this_value;
    if (machine.latched) {
        th_note(K5_NOTE_REFUSED_AFTER_LATCH, machine.calls);
        return refusal(ctx, "stopped");
    }
    if (machine.calls >= machine.call_ceiling) {
        th_note(K5_NOTE_PROGRAM_CEILING, 1);
        return refusal(ctx, "calls");
    }
    if (argc < 2) {
        return refusal(ctx, "bad_argument");
    }

    /* verb \0 argc \0 arg \0 arg \0 ... -- a shape with no escaping in it,
     * because every field's length is known before it is written. */
    uint8_t request[K5_CHANNEL_REQUEST_MAX];
    uint32_t at = 0;
    size_t len = 0;
    const char *verb = JS_ToCStringLen(ctx, &len, argv[0]);
    if (!verb || len + 8 > sizeof(request)) {
        if (verb) { JS_FreeCString(ctx, verb); }
        return refusal(ctx, "bad_argument");
    }
    uint32_t width = (uint32_t) len;
    memcpy(request + at, &width, 4); at += 4;
    memcpy(request + at, verb, len); at += (uint32_t) len;
    JS_FreeCString(ctx, verb);

    uint32_t count = 0;
    JSValue length_value = JS_GetPropertyStr(ctx, argv[1], "length");
    int32_t declared = 0;
    JS_ToInt32(ctx, &declared, length_value);
    JS_FreeValue(ctx, length_value);
    if (declared < 0) { declared = 0; }
    if (declared > 8) { declared = 8; }
    uint32_t count_at = at;
    at += 4;
    for (int32_t index = 0; index < declared; index++) {
        JSValue item = JS_GetPropertyUint32(ctx, argv[1], (uint32_t) index);
        size_t item_len = 0;
        const char *text = JS_ToCStringLen(ctx, &item_len, item);
        JS_FreeValue(ctx, item);
        if (!text) { continue; }
        if (at + 4 + item_len > sizeof(request)) {
            JS_FreeCString(ctx, text);
            return refusal(ctx, "too_large");
        }
        uint32_t item_width = (uint32_t) item_len;
        memcpy(request + at, &item_width, 4); at += 4;
        memcpy(request + at, text, item_len); at += (uint32_t) item_len;
        JS_FreeCString(ctx, text);
        count++;
    }
    memcpy(request + count_at, &count, 4);

    uint8_t *answer = NULL;
    uint32_t answer_len = 0;
    machine.requests++;
    if (host_call(K5_HOST_OP_REQUEST, request, at, 0, 0, &answer, &answer_len, NULL) != 0) {
        return refusal(ctx, "refused");
    }
    return answer_value(ctx, answer, answer_len);
}

static JSValue bound_validate(JSContext *ctx, JSValueConst this_value,
                              int argc, JSValueConst *argv)
{
    (void) this_value;
    if (machine.latched) {
        th_note(K5_NOTE_REFUSED_AFTER_LATCH, machine.calls);
        return refusal(ctx, "stopped");
    }
    size_t len = 0;
    const char *text = argc > 0 ? JS_ToCStringLen(ctx, &len, argv[0]) : NULL;
    uint8_t *answer = NULL;
    uint32_t answer_len = 0;
    machine.validations++;
    int ok = host_call(K5_HOST_OP_VALIDATE, (const uint8_t *) (text ? text : ""),
                       (uint32_t) len, 0, 0, &answer, &answer_len, NULL);
    if (text) { JS_FreeCString(ctx, text); }
    if (ok != 0) {
        return refusal(ctx, "refused");
    }
    return answer_value(ctx, answer, answer_len);
}

static JSValue bound_changed(JSContext *ctx, JSValueConst this_value,
                             int argc, JSValueConst *argv)
{
    (void) this_value; (void) argc; (void) argv;
    if (machine.latched) {
        return refusal(ctx, "stopped");
    }
    uint8_t *answer = NULL;
    uint32_t answer_len = 0;
    machine.observations++;
    if (host_call(K5_HOST_OP_CHANGED, NULL, 0, 0, 0, &answer, &answer_len, NULL) != 0) {
        return refusal(ctx, "refused");
    }
    return answer_value(ctx, answer, answer_len);
}

static JSValue bound_model(JSContext *ctx, JSValueConst this_value,
                           int argc, JSValueConst *argv)
{
    (void) this_value;
    if (machine.latched) {
        return refusal(ctx, "stopped");
    }
    size_t len = 0;
    const char *prompt = argc > 0 ? JS_ToCStringLen(ctx, &len, argv[0]) : NULL;
    int32_t predict = 16;
    if (argc > 1) { JS_ToInt32(ctx, &predict, argv[1]); }
    uint8_t *answer = NULL;
    uint32_t answer_len = 0;
    machine.inferences++;
    int ok = host_call(K5_HOST_OP_MODEL, (const uint8_t *) (prompt ? prompt : ""),
                       (uint32_t) len, (uint64_t) predict, 0, &answer, &answer_len, NULL);
    if (prompt) { JS_FreeCString(ctx, prompt); }
    if (ok != 0) {
        return refusal(ctx, "refused");
    }
    return answer_value(ctx, answer, answer_len);
}

static JSValue bound_log(JSContext *ctx, JSValueConst this_value,
                         int argc, JSValueConst *argv)
{
    (void) this_value;
    size_t len = 0;
    const char *text = argc > 0 ? JS_ToCStringLen(ctx, &len, argv[0]) : NULL;
    if (text) {
        host_call(K5_HOST_OP_LOG, (const uint8_t *) text, (uint32_t) len, 0, 0, NULL, NULL, NULL);
        JS_FreeCString(ctx, text);
    }
    return JS_UNDEFINED;
}

/* The latch. Recorded here, where the program cannot reach it, before it
 * throws. Everything after this refuses. */
static JSValue bound_assert(JSContext *ctx, JSValueConst this_value,
                            int argc, JSValueConst *argv)
{
    (void) this_value;
    size_t len = 0;
    const char *what = argc > 0 ? JS_ToCStringLen(ctx, &len, argv[0]) : NULL;
    machine.assertions++;
    machine.latched = 1;
    machine.finish = K5_FINISH_ASSERTION;
    host_call(K5_HOST_OP_ASSERT_FAILED, (const uint8_t *) (what ? what : ""),
              (uint32_t) len, 0, 0, NULL, NULL, NULL);
    if (what) { JS_FreeCString(ctx, what); }
    th_note(K5_NOTE_PROGRAM_LATCHED, machine.calls);
    return JS_UNDEFINED;
}

static JSValue bound_needs_model(JSContext *ctx, JSValueConst this_value,
                                 int argc, JSValueConst *argv)
{
    (void) this_value; (void) argc; (void) argv;
    (void) ctx;
    machine.needs_model = 1;
    machine.finish = K5_FINISH_NEEDS_MODEL;
    return JS_UNDEFINED;
}

/* Every ceiling that is not the engine's own accounting. QuickJS calls this
 * during bytecode execution, so a loop that never allocates and never calls
 * anything still stops. */
static int interrupt(JSRuntime *runtime, void *opaque)
{
    (void) runtime; (void) opaque;
    machine.ticks++;
    if (machine.latched) {
        return 1;
    }
    if (machine.ticks > machine.tick_ceiling) {
        machine.finish = K5_FINISH_EXHAUSTED;
        th_note(K5_NOTE_PROGRAM_CEILING, 2);
        return 1;
    }
    if (th_now_ns() - machine.started_ns > machine.wall_ns) {
        machine.finish = K5_FINISH_EXHAUSTED;
        th_note(K5_NOTE_PROGRAM_CEILING, 3);
        return 1;
    }
    return 0;
}

/* ------------------------------------------------------------ the allocator */

static void *engine_calloc(void *opaque, size_t count, size_t size)
{
    (void) opaque;
    return calloc(count, size);
}
static void *engine_malloc(void *opaque, size_t size) { (void) opaque; return malloc(size); }
static void engine_free(void *opaque, void *ptr) { (void) opaque; free(ptr); }
static void *engine_realloc(void *opaque, void *ptr, size_t size)
{
    (void) opaque;
    return realloc(ptr, size);
}
static size_t engine_usable(const void *ptr) { return malloc_usable_size((void *) ptr); }

static const JSMallocFunctions ENGINE_ALLOCATOR = {
    engine_calloc, engine_malloc, engine_free, engine_realloc, engine_usable,
};

/* ------------------------------------------------------------------- the run */

static void report_finish(JSContext *ctx, JSValue value)
{
    size_t len = 0;
    const char *text = NULL;
    JSValue json = JS_UNDEFINED;
    if (!JS_IsUndefined(value) && !JS_IsException(value)) {
        json = JS_JSONStringify(ctx, value, JS_UNDEFINED, JS_UNDEFINED);
        if (!JS_IsException(json)) {
            text = JS_ToCStringLen(ctx, &len, json);
        }
    }
    if (len > K5_CHANNEL_REQUEST_MAX) {
        len = 0;
        machine.finish = K5_FINISH_EXHAUSTED;
        th_note(K5_NOTE_PROGRAM_CEILING, 4);
    }
    machine.returned_bytes = len;

    k5_run_metrics metrics;
    memset(&metrics, 0, sizeof(metrics));
    metrics.requests = (uint32_t) machine.requests;
    metrics.validations = (uint32_t) machine.validations;
    metrics.observations = (uint32_t) machine.observations;
    metrics.assertions = (uint32_t) machine.assertions;
    metrics.inferences = (uint32_t) machine.inferences;
    metrics.finish = machine.finish;
    metrics.answer_bytes = machine.answer_bytes;
    metrics.returned_bytes = machine.returned_bytes;
    metrics.wall_ns = th_now_ns() - machine.started_ns;

    uint8_t request[K5_CHANNEL_REQUEST_MAX];
    memcpy(request, &metrics, sizeof(metrics));
    uint32_t at = (uint32_t) sizeof(metrics);
    if (text && len > 0) {
        memcpy(request + at, text, len);
        at += (uint32_t) len;
    }
    host_call(K5_HOST_OP_FINISH, request, at, machine.ticks, 0, NULL, NULL, NULL);
    if (text) { JS_FreeCString(ctx, text); }
    JS_FreeValue(ctx, json);
    th_note(K5_NOTE_PROGRAM_FINISH, machine.finish);
}

int th_main(const th_config *config)
{
    machine.facet = thalyx_boot_handle_of(TH_SLOT_SERVICE);
    machine.channel = (uint8_t *) (uintptr_t) TH_SHARED_VADDR;
    machine.started_ns = th_now_ns();
    machine.wall_ns = config->arg0 ? config->arg0 : 60ull * 1000 * 1000 * 1000;
    machine.tick_ceiling = config->arg1 ? config->arg1 : 20ull * 1000 * 1000;
    machine.call_ceiling = config->arg2 ? config->arg2 : 512;
    machine.finish = K5_FINISH_RETURNED;

    JSRuntime *runtime = JS_NewRuntime2(&ENGINE_ALLOCATOR, NULL);
    if (!runtime) {
        th_note(K5_NOTE_PROGRAM_REFUSED, 1);
        return 1;
    }
    /* The engine's own ceiling, below what the scope can charge. Which one a
     * run hit is then a fact the record distinguishes. */
    JS_SetMemoryLimit(runtime, (size_t) (config->arg3 ? config->arg3 : 2u * 1024 * 1024));
    JS_SetMaxStackSize(runtime, 512 * 1024);
    JS_SetInterruptHandler(runtime, interrupt, NULL);

    JSContext *ctx = JS_NewContext(runtime);
    if (!ctx) {
        th_note(K5_NOTE_PROGRAM_REFUSED, 2);
        JS_FreeRuntime(runtime);
        return 1;
    }

    JSValue global = JS_GetGlobalObject(ctx);
    JSValue surface = JS_NewObject(ctx);
    JS_SetPropertyStr(ctx, surface, "__call", JS_NewCFunction(ctx, bound_call, "__call", 2));
    JS_SetPropertyStr(ctx, surface, "__validate",
                      JS_NewCFunction(ctx, bound_validate, "__validate", 1));
    JS_SetPropertyStr(ctx, surface, "__changed",
                      JS_NewCFunction(ctx, bound_changed, "__changed", 0));
    JS_SetPropertyStr(ctx, surface, "__model", JS_NewCFunction(ctx, bound_model, "__model", 2));
    JS_SetPropertyStr(ctx, surface, "__log", JS_NewCFunction(ctx, bound_log, "__log", 1));
    JS_SetPropertyStr(ctx, surface, "__assert",
                      JS_NewCFunction(ctx, bound_assert, "__assert", 2));
    JS_SetPropertyStr(ctx, surface, "__needs_model",
                      JS_NewCFunction(ctx, bound_needs_model, "__needs_model", 1));
    JS_SetPropertyStr(ctx, global, "thalyx", surface);
    JS_FreeValue(ctx, global);

    JSValue prelude = JS_Eval(ctx, PRELUDE, strlen(PRELUDE), "<prelude>", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(prelude)) {
        th_note(K5_NOTE_PROGRAM_REFUSED, 3);
        JS_FreeValue(ctx, prelude);
        JS_FreeContext(ctx);
        JS_FreeRuntime(runtime);
        return 1;
    }
    JS_FreeValue(ctx, prelude);

    /* The program, from the region the work wrote it into. Its length is the
     * work's to state; this reads no further than it says. */
    const char *source = (const char *) (machine.channel + K5_CHANNEL_PROGRAM_OFFSET);
    size_t source_len = (size_t) config->shared_bytes;
    if (source_len > K5_CHANNEL_PROGRAM_MAX) {
        source_len = K5_CHANNEL_PROGRAM_MAX;
    }
    size_t real = strnlen(source, source_len);
    th_note(K5_NOTE_PROGRAM_COMPILED, real);

    /* Wrapped, so every way the program can end is a value on this side. The
     * two endings the language can see -- returning and throwing -- are told
     * apart in the language; the two it cannot -- a ceiling and a latch -- are
     * told apart here. */
    static char wrapped[K5_CHANNEL_PROGRAM_MAX + 512];
    int written = snprintf(wrapped, sizeof(wrapped),
        "(function(){\"use strict\";try{const v=(function(){%.*s\n})();"
        "return {kind:\"returned\",value:v===undefined?null:v};}"
        "catch(e){return {kind:\"threw\",message:String(e&&e.message?e.message:e)};}})()",
        (int) real, source);
    if (written < 0 || (size_t) written >= sizeof(wrapped)) {
        th_note(K5_NOTE_PROGRAM_REFUSED, 4);
        JS_FreeContext(ctx);
        JS_FreeRuntime(runtime);
        return 1;
    }

    JSValue outcome = JS_Eval(ctx, wrapped, (size_t) written, "<program>", JS_EVAL_TYPE_GLOBAL);
    JSValue value = JS_UNDEFINED;
    if (JS_IsException(outcome)) {
        /* The engine stopped it: a ceiling, or the latch. `machine.finish`
         * already says which, and a run that got here without one is a fault of
         * the engine rather than of the program. */
        JSValue exception = JS_GetException(ctx);
        JS_FreeValue(ctx, exception);
        if (machine.finish == K5_FINISH_RETURNED) {
            machine.finish = K5_FINISH_THREW;
        }
    } else {
        JSValue kind = JS_GetPropertyStr(ctx, outcome, "kind");
        const char *word = JS_ToCString(ctx, kind);
        int threw = word && strcmp(word, "threw") == 0;
        if (threw && machine.finish == K5_FINISH_RETURNED) {
            machine.finish = K5_FINISH_THREW;
        }
        if (word) { JS_FreeCString(ctx, word); }
        JS_FreeValue(ctx, kind);
        /* What comes back is what the run produced: the value when it returned,
         * and the reason when it threw. A throw whose reason was dropped is a
         * failure nobody can act on, which is the shape of diagnosis this whole
         * arrangement exists to avoid. */
        if (threw) {
            value = JS_GetPropertyStr(ctx, outcome, "message");
            size_t len = 0;
            const char *said = JS_ToCStringLen(ctx, &len, value);
            if (said) {
                for (size_t base = 0; base < len && base < 48; base += 8) {
                    uint64_t packed = 0;
                    for (size_t index = base; index < len && index < base + 8; index++) {
                        packed = (packed << 8) | (uint8_t) said[index];
                    }
                    th_note(K5_NOTE_PROGRAM_REFUSED, packed);
                }
                JS_FreeCString(ctx, said);
            }
        } else {
            value = JS_GetPropertyStr(ctx, outcome, "value");
        }
    }

    report_finish(ctx, value);
    JS_FreeValue(ctx, value);
    JS_FreeValue(ctx, outcome);
    JS_FreeContext(ctx);
    JS_FreeRuntime(runtime);
    return machine.finish == K5_FINISH_RETURNED ? 0 : 2;
}
