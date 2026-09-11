/* The native half of the K6 paired benchmarks.
 *
 * This file is what bench.c needs from this kernel and nothing else: a kernel
 * entry for each primitive a benchmark measures, the threads a supervisor
 * built, a signal to block on, and a way to hand an entry to the supervisor
 * when running it takes an authority this program does not hold -- building a
 * domain, fencing a scope, reading the control log. What a domain is, is not
 * its choice: the supervisor writes the role into the boot record and installs
 * the capabilities that role gets, in the slots `k6.h` names.
 *
 *   CLIENT  runs the plan in the shared page. Its initial thread runs the
 *           entries; its built threads are the helpers `sched.wake` and the
 *           scaling benchmarks use.
 *   SERVER  answers calls on up to four endpoints, one thread each: it closes
 *           whatever capabilities a call carried and echoes the payload back.
 *   SPIN    the thread that never blocks, in a scope with a budget, for
 *           `quota.share`.
 *   ASKER   asks the engine for a long answer and is closed while it computes,
 *           for `engine.cancel`.
 *   IDLE    blocks in the kernel until it is stopped, for `closure.unit`.
 */

#include <string.h>

#include "bench.h"
#include "thalyx/k5.h"
#include "thalyx/nrt.h"
#include "thalyx/sys.h"

static const th_config *boot;
static k6_handoff *handoff;

static uint64_t slot(uint64_t index) { return thalyx_boot_handle_of((uint32_t)index); }

/* ------------------------------------------------------ backend basics */

uint64_t plat_now_ns(void) { return th_now_ns(); }

void plat_emit(uint64_t code, uint64_t value) { th_note(code, value); }

unsigned plat_helper_threads(void) { return boot ? (unsigned)boot->arg0 : 0u; }

int plat_thread_start(unsigned index, bench_fn body, void *argument)
{
    return th_thread_start(index, body, argument);
}

int plat_thread_join(unsigned index) { return th_thread_join(index); }

void plat_block(void)
{
    while (th_signal_wait(TH_SLOT_SIGNAL_WORK, K6_WAKE_BIT, 0) != THALYX_STATUS_OK) { }
}

void plat_wake(void) { th_signal_raise(TH_SLOT_SIGNAL_WORK, K6_WAKE_BIT); }

/* ------------------------------------------------------ kernel objects */

static int64_t create_memory(uint64_t pages, uint32_t rights, uint64_t *out)
{
    thalyx_memory_create_request_t create;
    memset(&create, 0, sizeof(create));
    create.pages = pages;
    create.max_rights = rights;
    memcpy(create.label, "k6bench", 7);
    th_desc d;
    th_desc_begin(&d, THALYX_OP_SCOPE_CREATE_MEMORY);
    th_desc_put(&d, TH_BODY, &create, sizeof(create));
    th_result r = th_op(slot(TH_SLOT_SELF_SCOPE), THALYX_OP_SCOPE_CREATE_MEMORY, &d, 0);
    if (r.status != THALYX_STATUS_OK) { return r.status; }
    *out = r.aux;
    return 0;
}

static int64_t map_memory(uint64_t memory, uint64_t vaddr, uint32_t pages, uint32_t rights)
{
    thalyx_map_request_t map;
    memset(&map, 0, sizeof(map));
    map.memory_handle = memory;
    map.vaddr = vaddr;
    map.page_count = pages;
    map.rights = rights;
    th_desc d;
    th_desc_begin(&d, THALYX_OP_DOMAIN_MAP);
    th_desc_put(&d, TH_BODY, &map, sizeof(map));
    return th_op(slot(TH_SLOT_SELF_DOMAIN), THALYX_OP_DOMAIN_MAP, &d, 0).status;
}

static int64_t unmap_memory(uint64_t vaddr, uint32_t pages)
{
    thalyx_unmap_request_t unmap;
    memset(&unmap, 0, sizeof(unmap));
    unmap.vaddr = vaddr;
    unmap.page_count = pages;
    th_desc d;
    th_desc_begin(&d, THALYX_OP_DOMAIN_UNMAP);
    th_desc_put(&d, TH_BODY, &unmap, sizeof(unmap));
    return th_op(slot(TH_SLOT_SELF_DOMAIN), THALYX_OP_DOMAIN_UNMAP, &d, 0).status;
}

static int64_t seal_memory(uint64_t memory)
{
    th_desc d;
    th_desc_begin(&d, THALYX_OP_MEMORY_SEAL);
    return th_op(memory, THALYX_OP_MEMORY_SEAL, &d, 0).status;
}

static int64_t derive(uint64_t handle, uint32_t rights, uint64_t *out)
{
    thalyx_derive_request_t request;
    memset(&request, 0, sizeof(request));
    request.rights_mask = rights;
    th_desc d;
    th_desc_begin(&d, THALYX_OP_CAP_DERIVE);
    th_desc_put(&d, TH_BODY, &request, sizeof(request));
    th_result r = th_op(handle, THALYX_OP_CAP_DERIVE, &d, 0);
    if (r.status != THALYX_STATUS_OK) { return r.status; }
    *out = r.aux;
    return 0;
}

static uint32_t derive_depth(uint64_t handle)
{
    th_desc d;
    th_desc_begin(&d, THALYX_OP_CAP_INSPECT);
    th_result r = th_op(handle, THALYX_OP_CAP_INSPECT, &d, 0);
    if (r.status != THALYX_STATUS_OK) { return 0; }
    thalyx_cap_info_t info;
    memcpy(&info, d.bytes + TH_BODY, sizeof(info));
    return info.derive_depth;
}

/* ---------------------------------------------------------------- IPC */

static uint64_t pair_facet(unsigned pair)
{
    static const uint64_t slots[4] = {K6_SLOT_PAIR0, K6_SLOT_PAIR1, K6_SLOT_PAIR2, K6_SLOT_PAIR3};
    return pair < 4 ? slot(slots[pair]) : 0;
}

/* One call: `payload` bytes out, the same number expected back, and `count`
 * capabilities copied to the answerer, which closes them. The descriptor is
 * built for every call because the kernel writes the answer into it; that is
 * part of what a call costs a native program, and it is counted. */
static int64_t call_raw(uint64_t facet, uint32_t payload, const uint64_t *caps, uint32_t count)
{
    thalyx_send_request_t request;
    memset(&request, 0, sizeof(request));
    request.payload_len = payload;
    for (uint32_t i = 0; i < payload; i++) { request.payload[i] = (uint8_t)i; }
    request.cap_count = count;
    for (uint32_t i = 0; i < count; i++) {
        request.caps[i] = caps[i];
        request.cap_ops[i] = THALYX_CAP_OP_COPY;
    }
    th_desc d;
    th_desc_begin(&d, THALYX_OP_ENDPOINT_CALL);
    th_desc_put(&d, TH_BODY, &request, sizeof(request));
    th_result r = th_op(facet, THALYX_OP_ENDPOINT_CALL, &d, 0);
    if (r.status != THALYX_STATUS_OK) { return r.status; }
    thalyx_call_result_t result;
    memcpy(&result, d.bytes + TH_BODY, sizeof(result));
    return result.payload_len == payload ? 0 : THALYX_STATUS_STATE_CONFLICT;
}

int64_t plat_ipc_pair_call(unsigned pair) { return call_raw(pair_facet(pair), 0, NULL, 0); }

/* -------------------------------------------------------------- entries */

static uint64_t cap_objects[4];
static uint32_t cap_count;
static uint64_t derive_source;
static uint64_t lineage;
static uint64_t prompt_memory;

static const uint32_t MEMORY_RW = THALYX_RIGHT_MEMORY_READ | THALYX_RIGHT_MEMORY_WRITE;

static int64_t engine_prepare(void)
{
    if (slot(K6_SLOT_ENGINE) == 0) { return THALYX_STATUS_INVALID_HANDLE; }
    if (prompt_memory != 0) { return 0; }
    uint64_t memory = 0;
    int64_t status = create_memory(2, MEMORY_RW | THALYX_RIGHT_MEMORY_MAP, &memory);
    if (status != 0) { return status; }
    status = map_memory(memory, K6_PROMPT_VADDR, 2, MEMORY_RW);
    if (status != 0) {
        /* Not kept: a buffer that is not mapped is not a prompt buffer, and a
         * later entry that took it for one wrote through nothing. */
        th_close(memory);
        return status;
    }
    prompt_memory = memory;
    return 0;
}

int64_t plat_prepare(uint32_t bench, uint32_t param)
{
    switch (bench) {
    case K6_BENCH_IPC_CALL:
        if (pair_facet(0) == 0) { return THALYX_STATUS_INVALID_HANDLE; }
        return param <= THALYX_MAX_INLINE_PAYLOAD ? 0 : THALYX_STATUS_INVALID_ARGUMENT;
    case K6_BENCH_IPC_CAPS:
        if (param == 0 || param > 4) { return THALYX_STATUS_INVALID_ARGUMENT; }
        cap_count = 0;
        for (uint32_t i = 0; i < param; i++) {
            int64_t status = create_memory(1, MEMORY_RW | THALYX_RIGHT_MEMORY_MAP, &cap_objects[i]);
            if (status != 0) { plat_release(bench, param); return status; }
            cap_count++;
        }
        return 0;
    case K6_BENCH_CAP_DERIVE:
        return create_memory(1, MEMORY_RW | THALYX_RIGHT_MEMORY_MAP, &derive_source);
    case K6_BENCH_IPC_LINEAGE: {
        uint64_t facet = pair_facet(0);
        if (facet == 0) { return THALYX_STATUS_INVALID_HANDLE; }
        uint64_t current = facet;
        for (uint32_t step = 0; step < param; step++) {
            uint64_t next = 0;
            int64_t status = derive(current, THALYX_RIGHT_INSPECT | THALYX_RIGHT_TRANSFER
                                                 | THALYX_RIGHT_DERIVE | THALYX_RIGHT_ENDPOINT_CALL,
                                    &next);
            if (current != facet) { th_close(current); }
            if (status != 0) { return status; }
            current = next;
        }
        lineage = current;
        /* The depth the kernel walks, which is the facet's own plus the steps
         * taken here: said beside the samples rather than assumed. */
        plat_emit(K6_NOTE_AUX, K6_BENCH_IPC_LINEAGE | ((uint64_t)param << 16)
                                   | ((uint64_t)derive_depth(lineage) << 32));
        return 0;
    }
    case K6_BENCH_ENGINE_INFER:
        return engine_prepare();
    default:
        return 0;
    }
}

int64_t plat_op(uint32_t bench, uint32_t param)
{
    switch (bench) {
    case K6_BENCH_ENTRY_VERSION:
        return th_abi_version() != 0 ? 0 : THALYX_STATUS_UNSUPPORTED_ENTRY;
    case K6_BENCH_ENTRY_CLOCK:
        return th_now_ns() != 0 ? 0 : THALYX_STATUS_UNSUPPORTED_ENTRY;
    case K6_BENCH_IPC_CALL:
        return call_raw(pair_facet(0), param, NULL, 0);
    case K6_BENCH_IPC_CAPS:
        return call_raw(pair_facet(0), 0, cap_objects, cap_count);
    case K6_BENCH_IPC_LINEAGE:
        return call_raw(lineage, 0, NULL, 0);
    case K6_BENCH_MEM_MAP: {
        uint64_t memory = 0;
        int64_t status = create_memory(param, MEMORY_RW | THALYX_RIGHT_MEMORY_MAP, &memory);
        if (status != 0) { return status; }
        status = map_memory(memory, K6_MAP_VADDR, param, MEMORY_RW);
        if (status == 0) {
            for (uint32_t page = 0; page < param; page++) {
                ((volatile uint8_t *)(uintptr_t)K6_MAP_VADDR)[(uint64_t)page * 4096u] = (uint8_t)page;
            }
            status = unmap_memory(K6_MAP_VADDR, param);
        }
        th_close(memory);
        return status;
    }
    case K6_BENCH_MEM_SEAL: {
        uint64_t memory = 0;
        const uint32_t pages = param;
        int64_t status = create_memory(pages, MEMORY_RW | THALYX_RIGHT_MEMORY_MAP
                                                  | THALYX_RIGHT_MEMORY_SEAL, &memory);
        if (status != 0) { return status; }
        status = map_memory(memory, K6_SEAL_VADDR, pages, MEMORY_RW);
        if (status == 0) {
            for (uint32_t page = 0; page < pages; page++) {
                ((volatile uint8_t *)(uintptr_t)K6_SEAL_VADDR)[(uint64_t)page * 4096u] = (uint8_t)(page + 1);
            }
            /* The seal withdraws the writable mapping itself and does not
             * answer until every processor the domain ran on has retired it. */
            status = seal_memory(memory);
        }
        const uint64_t readonly = K6_SEAL_VADDR + 0x100000u;
        if (status == 0) { status = map_memory(memory, readonly, pages, THALYX_RIGHT_MEMORY_READ); }
        if (status == 0) {
            if (((volatile uint8_t *)(uintptr_t)readonly)[0] != 1) { status = THALYX_STATUS_STATE_CONFLICT; }
            int64_t unmapped = unmap_memory(readonly, pages);
            if (status == 0) { status = unmapped; }
        }
        th_close(memory);
        return status;
    }
    case K6_BENCH_CAP_DERIVE: {
        uint64_t derived = 0;
        int64_t status = derive(derive_source, THALYX_RIGHT_INSPECT | THALYX_RIGHT_MEMORY_READ, &derived);
        if (status != 0) { return status; }
        return th_close(derived);
    }
    default:
        return THALYX_STATUS_NOT_SUPPORTED;
    }
}

void plat_release(uint32_t bench, uint32_t param)
{
    (void)param;
    switch (bench) {
    case K6_BENCH_IPC_CAPS:
        for (uint32_t i = 0; i < cap_count; i++) { th_close(cap_objects[i]); }
        cap_count = 0;
        break;
    case K6_BENCH_CAP_DERIVE:
        if (derive_source) { th_close(derive_source); }
        derive_source = 0;
        break;
    case K6_BENCH_IPC_LINEAGE:
        if (lineage && lineage != pair_facet(0)) { th_close(lineage); }
        lineage = 0;
        break;
    default:
        break;
    }
}

/* --------------------------------------------------------------- engine */

/* One question to the resident engine. The prompt travels in this program's
 * own buffer, lent with the call and narrowed on the way; the answer comes
 * back in the same buffer, which is how a K5 work asks, and the digest of the
 * answer's bytes is what the host compares with the reference. */
static int64_t ask_engine(unsigned index, const char *grammar, uint64_t *digest, uint32_t *length)
{
    const char *prompt = k6_engine_prompt[index];
    uint32_t len = (uint32_t)strlen(prompt);
    uint32_t grammar_len = grammar ? (uint32_t)strlen(grammar) : 0;
    memcpy((void *)(uintptr_t)K6_PROMPT_VADDR, prompt, len);
    if (grammar_len) { memcpy((void *)(uintptr_t)(K6_PROMPT_VADDR + len), grammar, grammar_len); }
    k5_engine_request request;
    memset(&request, 0, sizeof(request));
    request.op = K5_ENGINE_OP_INFER;
    request.predict = k6_engine_predict[index];
    request.prompt_len = len;
    request.grammar_len = grammar_len;
    th_payload out;
    memset(&out, 0, sizeof(out));
    memcpy(out.bytes, &request, sizeof(request));
    out.len = sizeof(request);
    th_payload in;
    memset(&in, 0, sizeof(in));
    int64_t result = th_call(slot(K6_SLOT_ENGINE), &out, &in, prompt_memory,
                             THALYX_RIGHT_INSPECT | THALYX_RIGHT_TRANSFER | MEMORY_RW,
                             th_now_ns() + 120000000000ull);
    if (result < 0) { return result; }
    if (in.len < sizeof(k5_engine_reply)) { return THALYX_STATUS_STATE_CONFLICT; }
    k5_engine_reply reply;
    memcpy(&reply, in.bytes, sizeof(reply));
    if (reply.status != K5_ENGINE_STATUS_OK) { return -(int64_t)(100 + reply.status); }
    uint32_t answer = reply.answer_len > K5_ANSWER_MAX ? K5_ANSWER_MAX : reply.answer_len;
    *digest = bench_fnv1a((const void *)(uintptr_t)K6_PROMPT_VADDR, answer);
    *length = answer;
    return 0;
}

static int64_t engine_infer(uint32_t index, uint32_t *out, uint32_t n)
{
    if (index >= K6_ENGINE_PROMPTS) { return THALYX_STATUS_INVALID_ARGUMENT; }
    uint64_t hz = bench_tsc_hz();
    uint64_t first = 0;
    uint32_t first_length = 0, same = 0, differ = 0, errors = 0;
    for (uint32_t i = 0; i < n; i++) {
        uint64_t digest = 0;
        uint32_t length = 0;
        uint64_t c0 = bench_cycles();
        int64_t status = ask_engine(index, NULL, &digest, &length);
        uint64_t c1 = bench_cycles();
        if (status != 0) {
            if (errors++ == 0) {
                plat_emit(K6_NOTE_ERROR, K6_BENCH_ENGINE_INFER | ((uint64_t)(uint32_t)status << 32));
            }
            out[i] = (uint32_t)K6_LOST_SAMPLE;
            continue;
        }
        uint64_t us = (uint64_t)(((unsigned __int128)(c1 - c0) * 1000000u) / hz);
        out[i] = us >= K6_LOST_SAMPLE ? (uint32_t)(K6_LOST_SAMPLE - 1) : (uint32_t)us;
        if (same + differ == 0) {
            first = digest;
            first_length = length;
        }
        if (digest == first) { same++; } else { differ++; }
    }
    plat_emit(K6_NOTE_AUX, K6_BENCH_ENGINE_INFER | ((uint64_t)index << 16) | ((uint64_t)first_length << 32));
    plat_emit(K6_NOTE_DIGEST, first);
    plat_emit(K6_NOTE_CHECK, K6_BENCH_ENGINE_INFER | ((uint64_t)same << 32));
    if (differ) { plat_emit(K6_NOTE_MISMATCH, index); }
    return (int64_t)n;
}

/* ------------------------------------------------------------- handoff */

/* An entry this program cannot run is run by its supervisor, in the plan's
 * order: the request goes in the shared page, a bit says it is there, and the
 * program blocks until the supervisor says it is done. The supervisor's
 * samples land between this entry's BEGIN and END. */
static int64_t hand_off(uint32_t bench, uint32_t param, uint32_t samples)
{
    if (handoff == NULL || slot(K6_SLOT_HANDOFF) == 0) { return THALYX_STATUS_INVALID_HANDLE; }
    handoff->bench = bench;
    handoff->param = param;
    handoff->samples = samples;
    handoff->status = 0;
    __atomic_thread_fence(__ATOMIC_RELEASE);
    int64_t status = th_signal_raise(K6_SLOT_HANDOFF, K6_HANDOFF_REQUEST_BIT);
    if (status != THALYX_STATUS_OK) { return status; }
    status = th_signal_wait(K6_SLOT_HANDOFF, K6_HANDOFF_DONE_BIT, 0);
    if (status != THALYX_STATUS_OK) { return status; }
    __atomic_thread_fence(__ATOMIC_ACQUIRE);
    return handoff->status ? -(int64_t)handoff->status : 0;
}

int64_t plat_special(uint32_t bench, uint32_t param, uint32_t *out, uint32_t max)
{
    switch (bench) {
    case K6_BENCH_SCHED_WAKE:
        return bench_wake(out, max);
    case K6_BENCH_SCALE_COMPUTE:
        return bench_scale_compute(param, out, max);
    case K6_BENCH_SCALE_IPC:
        return bench_scale_ipc(param, out, max);
    case K6_BENCH_ENGINE_INFER:
        return engine_infer(param, out, max);
    case K6_BENCH_QUOTA_SHARE:
    case K6_BENCH_CLOSURE_UNIT:
    case K6_BENCH_ENGINE_LOAD:
    case K6_BENCH_ENGINE_CANCEL:
    case K6_BENCH_AUDIT_DRAIN:
        return hand_off(bench, param, max);
    default:
        return THALYX_STATUS_NOT_SUPPORTED;
    }
}

/* --------------------------------------------------------------- roles */

static void serve(void *argument)
{
    static const uint64_t slots[4] = {TH_SLOT_INBOUND, K6_SLOT_PAIR1, K6_SLOT_PAIR2, K6_SLOT_PAIR3};
    unsigned index = (unsigned)(uintptr_t)argument;
    uint64_t endpoint = slot(slots[index & 3]);
    for (;;) {
        th_message message;
        int64_t status = th_receive(endpoint, &message, 0);
        if (status != THALYX_STATUS_OK) {
            if (status == THALYX_STATUS_PEER_DEAD || status == THALYX_STATUS_SCOPE_CLOSED) { return; }
            continue;
        }
        for (uint32_t i = 0; i < message.lent_count; i++) { th_close(message.lent[i]); }
        th_payload body;
        body.len = message.payload.len;
        memcpy(body.bytes, message.payload.bytes, body.len);
        th_reply(message.invocation, 0, &body);
        th_close(message.invocation);
    }
}

static int server(void)
{
    unsigned endpoints = (unsigned)boot->arg1;
    if (endpoints == 0 || endpoints > 4) { endpoints = 1; }
    for (unsigned t = 1; t < endpoints; t++) { th_thread_start(t, serve, (void *)(uintptr_t)t); }
    serve((void *)(uintptr_t)0);
    return 0;
}

static uint32_t spin_samples[1024];

static int spin(void)
{
    bench_calibrate();
    int64_t n = bench_spin_share(boot->arg1, spin_samples, 1024);
    if (n > 0) { bench_emit_samples(spin_samples, (uint32_t)n); }
    return 0;
}

static int asker(void)
{
    if (engine_prepare() != 0) { return 2; }
    th_signal_raise(K6_SLOT_HANDOFF, K6_ASKER_ASKING_BIT);
    uint64_t digest = 0;
    uint32_t length = 0;
    /* Four hundred tokens under a grammar that never accepts an end: the
     * fixture's long prompt ends after a few tokens on this model, and a
     * request that has already been answered cannot be cancelled. */
    int64_t status = ask_engine(K6_ENGINE_PROMPTS - 1, K6_CANCEL_GRAMMAR, &digest, &length);
    plat_emit(K6_NOTE_AUX, K6_BENCH_ENGINE_CANCEL | (1ull << 16) | ((uint64_t)(uint32_t)status << 32));
    return 0;
}

/* Blocked in the kernel until stopped: a wait on the work signal, which this
 * role may wait on and nobody raises. It says it is about to block first, so
 * the supervisor times the closure of a blocked domain and not of one still
 * starting: the runtime's own startup note is a diagnostic record, and under
 * KVM a record costs the processor writing it a few milliseconds, which the
 * first version of this benchmark measured as the cost of closure. */
static int idle(void)
{
    th_signal_raise(TH_SLOT_SIGNAL_DONE, K6_IDLE_READY_BIT);
    for (;;) { th_signal_wait(TH_SLOT_SIGNAL_WORK, K6_WAKE_BIT, 0); }
}

int th_main(const th_config *config)
{
    boot = config;
    switch (config->role) {
    case K6_ROLE_CLIENT:
        handoff = (k6_handoff *)(uintptr_t)(TH_SHARED_VADDR + K6_SHARED_HANDOFF_OFFSET);
        bench_run_plan((const k6_plan *)(uintptr_t)(TH_SHARED_VADDR + K6_SHARED_PLAN_OFFSET));
        return 0;
    case K6_ROLE_SERVER:
        return server();
    case K6_ROLE_SPIN:
        return spin();
    case K6_ROLE_ASKER:
        return asker();
    case K6_ROLE_IDLE:
        return idle();
    default:
        return 1;
    }
}
