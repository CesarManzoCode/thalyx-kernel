// The resident inference engine, on this kernel.
//
// A port of `engine/thalyx-engine.cpp` from the Thalyx revision this port is
// against; `tools/fetch_engine.py` pins that file and the host comparison
// builds it unchanged. `serve_one` below is that file's `serve_one`: the same
// llama.cpp `common` calls, in the same order, with the same parameters. The
// context memory is cleared and the sampler is built fresh for every request,
// the temperature is zero, the prompt is decoded in batches, the prompt's own
// tokens pass through the sampler without being counted, and generation stops
// at an end-of-generation token or the budget. Residency is about the weights,
// not the conversation, and that is Thalyx's decision, kept.
//
// What changes is everything around it, and each change is one the kernel
// makes possible rather than one this file asserts:
//
//   * The transport. Thalyx frames requests on a pipe. Here a request is an
//     invocation on an endpoint the supervisor gave this domain, so who asked
//     is in the header the kernel wrote, not in a field of the request.
//   * The prompt. Thalyx sends a path and the engine opens a file. Here the
//     caller lends a buffer with the invocation and the engine reaches it
//     through that capability and nothing else; it stops working when the
//     caller's scope is closed.
//   * The weights. Thalyx passes `-m <path>`. Here they are the sealed bulk
//     object the supervisor mapped read-only, opened as `/bulk` -- the only
//     name this domain can open, because it is the only thing mapped into it.
//     llama.cpp's loader has no `mmap` to use here and reads the tensors into
//     buffers this engine allocates, so the resident weights are pages charged
//     to this domain's own scope.
//   * Who pays. The worker binds to the invocation before it computes, so the
//     kernel charges the inference to the scope of the work that asked; the
//     weights and everything loaded once stay charged here. Thalyx measures
//     neither. Here both are the kernel's numbers.
//   * Cancellation. Between tokens the engine asks the kernel whether the
//     caller's scope is still open. If it is not, the engine stops and answers
//     CANCELLED rather than spend a closure reserve on an answer nobody will
//     read, and it stays resident for the next caller.
//
// The answer is the completion without Thalyx's echo of the prompt. The echo
// is how Thalyx tells "the engine never read the prompt" from "the model
// answered badly"; here the engine reports a digest of the exact bytes it read
// through the capability, and the caller compares it with what it lent.

#include "common.h"
#include "sampling.h"
#include "llama.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <exception>
#include <string>
#include <vector>

#include "thalyx/nrt.h"
#include "thalyx/k5.h"

namespace {

// `bit::ENGINE_READY` in user/k5pkg/src/proto.rs.
constexpr uint64_t READY_BIT = 1ull << 7;
// The signal the supervisor waits on for that bit.
constexpr uint32_t SLOT_READY = TH_SLOT_AUX0;

// FNV-1a's 64-bit offset basis, 0xcbf29ce484222325: the same digest the work
// computes over the prompt it lent and the host computes over the model file.
constexpr uint64_t FNV_OFFSET = 14695981039346656037ull;

uint64_t fnv(const void *bytes, size_t len, uint64_t hash = FNV_OFFSET)
{
    const uint8_t *at = static_cast<const uint8_t *>(bytes);
    for (size_t i = 0; i < len; i++) {
        hash = (hash ^ at[i]) * 1099511628211ull;
    }
    return hash;
}

uint64_t fnv(const std::string &text) { return fnv(text.data(), text.size()); }

struct Resident {
    llama_model *model = nullptr;
    llama_context *ctx = nullptr;
    int32_t n_batch = 512;
    uint64_t load_ns = 0;
    uint64_t weight_bytes = 0;
    uint64_t served = 0;
};

// What llama.cpp and ggml print goes nowhere: the diagnostic plane is eight
// bytes a note, and a model card would be hundreds of them. The last warning
// is kept so a failure can be answered with its reason, as Thalyx answers one,
// and the rest is counted.
char last_warning[240];
uint64_t log_lines;

void log_sink(ggml_log_level level, const char *text, void *)
{
    log_lines++;
    if (level == GGML_LOG_LEVEL_WARN || level == GGML_LOG_LEVEL_ERROR) {
        std::strncpy(last_warning, text, sizeof(last_warning) - 1);
        last_warning[sizeof(last_warning) - 1] = 0;
    }
    // Errors are few and are the one thing worth the plane's cost: without
    // them a failure arrives as an assertion that names a line and not a size.
    if (level == GGML_LOG_LEVEL_ERROR) {
        std::fputs(text, stderr);
    }
}

// Whether the caller's scope has been closed since it asked. A query the
// kernel refuses is treated as closed: an engine that cannot confirm its
// caller is still there has no one to keep computing for.
bool caller_gone(uint64_t invocation)
{
    th_desc d;
    th_desc_begin(&d, THALYX_OP_INVOCATION_QUERY);
    th_result r = th_op(invocation, THALYX_OP_INVOCATION_QUERY, &d, 0);
    if (r.status != THALYX_STATUS_OK) {
        return true;
    }
    thalyx_invocation_info_t info;
    std::memcpy(&info, d.bytes + TH_BODY, sizeof(info));
    return info.cancel_state != THALYX_CANCEL_STATE_LIVE;
}

struct Outcome {
    std::string answer;
    std::string why;
    uint32_t prompt_tokens = 0;
    uint32_t generated = 0;
    int32_t first_token = -1;
    int32_t first_argmax = -1;
    uint32_t first_margin_ppm = 0;
    uint64_t token_digest = FNV_OFFSET;
    bool too_long = false;
    bool cancelled = false;
};

// The first decision, measured from the raw logits before the sampler touches
// them: which token has the highest logit, and by how much its probability
// beats the runner-up's. Two engines whose arithmetic differs in the last bits
// can only be compared on a decision, and a decision is only comparable when
// it was not a tie -- which is what the margin says.
void measure_first(Resident &r, const llama_vocab *vocab, Outcome &out)
{
    const float *logits = llama_get_logits_ith(r.ctx, -1);
    const int32_t n = llama_vocab_n_tokens(vocab);
    if (logits == nullptr || n < 2) {
        return;
    }
    int32_t best = 0;
    int32_t second = 1;
    if (logits[second] > logits[best]) {
        std::swap(best, second);
    }
    for (int32_t i = 2; i < n; i++) {
        if (logits[i] > logits[best]) {
            second = best;
            best = i;
        } else if (logits[i] > logits[second]) {
            second = i;
        }
    }
    double denominator = 0.0;
    for (int32_t i = 0; i < n; i++) {
        denominator += std::exp(static_cast<double>(logits[i]) - logits[best]);
    }
    const double p_best = 1.0 / denominator;
    const double p_second = std::exp(static_cast<double>(logits[second]) - logits[best]) / denominator;
    out.first_argmax = best;
    out.first_margin_ppm = static_cast<uint32_t>(std::llround((p_best - p_second) * 1e6));
}

// Thalyx's `serve_one`, with the two measurements this port reports and a check
// for the caller between tokens.
bool serve_one(Resident &r,
               const std::string &prompt,
               const std::string &grammar,
               uint32_t predict,
               uint64_t seed,
               uint64_t invocation,
               Outcome &out)
{
    const llama_vocab *vocab = llama_model_get_vocab(r.model);

    llama_memory_clear(llama_get_memory(r.ctx), true);

    std::vector<llama_token> tokens = common_tokenize(vocab, prompt, true, true);
    if (tokens.empty()) {
        out.why = "the prompt tokenised to nothing";
        return false;
    }
    out.prompt_tokens = static_cast<uint32_t>(tokens.size());

    const uint32_t n_ctx = llama_n_ctx(r.ctx);
    if (tokens.size() + predict > n_ctx) {
        out.too_long = true;
        out.why = "the prompt is " + std::to_string(tokens.size()) + " tokens and the context is "
            + std::to_string(n_ctx) + "; raise the engine's context or shorten the prompt";
        return false;
    }

    common_params_sampling sp;
    sp.seed = static_cast<uint32_t>(seed);
    sp.temp = 0.0f;
    if (!grammar.empty()) {
        sp.grammar = common_grammar(COMMON_GRAMMAR_TYPE_USER, grammar);
    }

    common_sampler *smpl = common_sampler_init(r.model, sp);
    if (!smpl) {
        out.why = "the sampler would not initialise -- the grammar is probably not valid GBNF";
        return false;
    }

    for (size_t at = 0; at < tokens.size(); at += static_cast<size_t>(r.n_batch)) {
        const size_t take = std::min(static_cast<size_t>(r.n_batch), tokens.size() - at);
        if (llama_decode(r.ctx, llama_batch_get_one(tokens.data() + at, static_cast<int32_t>(take))) != 0) {
            common_sampler_free(smpl);
            out.why = "llama_decode failed on the prompt";
            return false;
        }
    }

    for (llama_token t : tokens) {
        common_sampler_accept(smpl, t, false);
    }

    for (uint32_t made = 0; made < predict; made++) {
        if (caller_gone(invocation)) {
            out.cancelled = true;
            break;
        }
        if (made == 0) {
            measure_first(r, vocab, out);
        }
        llama_token id = common_sampler_sample(smpl, r.ctx, -1);
        common_sampler_accept(smpl, id, true);
        if (made == 0) {
            out.first_token = id;
        }
        const uint32_t word = static_cast<uint32_t>(id);
        out.token_digest = fnv(&word, sizeof(word), out.token_digest);
        out.generated++;
        if (llama_vocab_is_eog(vocab, id)) {
            break;
        }
        out.answer += common_token_to_piece(r.ctx, id, false);
        if (out.answer.size() > K5_ANSWER_MAX) {
            break;
        }
        if (llama_decode(r.ctx, llama_batch_get_one(&id, 1)) != 0) {
            common_sampler_free(smpl);
            out.why = "llama_decode failed while generating";
            return false;
        }
    }

    common_sampler_free(smpl);
    return true;
}

// Answers, or -- when there is nobody left to answer -- discharges. A caller
// whose scope was closed has had its wait cancelled by the kernel, and a reply
// to it is refused; the obligation is this engine's until it says what became
// of the request, and what became of it is that it was abandoned.
void answer_with(uint64_t invocation, const k5_engine_reply &reply)
{
    if (reply.status != K5_ENGINE_STATUS_CANCELLED) {
        th_payload body;
        std::memset(&body, 0, sizeof(body));
        std::memcpy(body.bytes, &reply, sizeof(reply));
        body.len = sizeof(reply);
        if (th_reply(invocation, reply.status, &body) == THALYX_STATUS_OK) {
            return;
        }
    }
    th_resolve(invocation, THALYX_OUTCOME_ABORTED, reply.generated);
}

void serve_request(Resident &r, th_message &message)
{
    k5_engine_request request;
    std::memset(&request, 0, sizeof(request));
    std::memcpy(&request, message.payload.bytes,
                std::min<size_t>(sizeof(request), message.payload.len));

    k5_engine_reply reply;
    std::memset(&reply, 0, sizeof(reply));
    reply.load_ns = r.load_ns;
    reply.served = r.served;
    reply.weight_bytes = r.weight_bytes;

    const uint64_t invocation = message.invocation;
    const uint64_t lent = message.lent_count ? message.lent[0] : 0;
    auto finish = [&](uint32_t status) {
        reply.status = status;
        if (status != K5_ENGINE_STATUS_OK) {
            th_note(K5_NOTE_ENGINE_REFUSED, status);
        }
        if (lent) {
            th_close(lent);
        }
        answer_with(invocation, reply);
        th_close(invocation);
    };

    if (message.payload.len < sizeof(request)
        || (request.op != K5_ENGINE_OP_INFER && request.op != K5_ENGINE_OP_QUERY)) {
        finish(K5_ENGINE_STATUS_INVALID);
        return;
    }
    if (request.op == K5_ENGINE_OP_QUERY) {
        finish(K5_ENGINE_STATUS_OK);
        return;
    }
    if (lent == 0) {
        finish(K5_ENGINE_STATUS_NO_BUFFER);
        return;
    }
    if (request.prompt_len == 0 || request.prompt_len > K5_PROMPT_MAX
        || request.grammar_len > K5_PROMPT_MAX) {
        finish(K5_ENGINE_STATUS_INVALID);
        return;
    }

    // From here the kernel charges this thread to the caller's scope.
    const int64_t bound = th_bind_worker(invocation);
    th_note(K5_NOTE_ENGINE_BOUND,
            bound == THALYX_STATUS_OK ? message.header.invocation_id : static_cast<uint64_t>(bound));

    std::string prompt(request.prompt_len, '\0');
    std::string grammar(request.grammar_len, '\0');
    const int64_t read = th_memory_read(lent, 0, prompt.data(), request.prompt_len);
    const int64_t read_grammar = request.grammar_len
        ? th_memory_read(lent, request.prompt_len, grammar.data(), request.grammar_len)
        : 0;
    if (read != static_cast<int64_t>(request.prompt_len)
        || read_grammar != static_cast<int64_t>(request.grammar_len)) {
        if (bound == THALYX_STATUS_OK) {
            th_unbind_worker(invocation);
        }
        finish(caller_gone(invocation) ? K5_ENGINE_STATUS_CANCELLED : K5_ENGINE_STATUS_NO_BUFFER);
        return;
    }
    th_note(K5_NOTE_ENGINE_PROMPT, fnv(prompt));

    Outcome out;
    bool ok = false;
    const uint64_t began = th_now_ns();
    // Caught, for Thalyx's reason: the weights took seconds to load, and the
    // things that throw in here are things a caller handed over. There is
    // nothing to recover inside the request; the answer is why it failed.
    try {
        ok = serve_one(r, prompt, grammar, request.predict, request.seed, invocation, out);
    } catch (const std::exception &e) {
        ok = false;
        out.why = std::string("the engine could not run that: ") + e.what();
        th_note(K5_NOTE_ENGINE_EXCEPTION, fnv(e.what(), std::strlen(e.what())));
    }
    reply.elapsed_ns = th_now_ns() - began;

    uint32_t status = K5_ENGINE_STATUS_OK;
    if (out.cancelled) {
        status = K5_ENGINE_STATUS_CANCELLED;
        th_note(K5_NOTE_ENGINE_CANCELLED, out.generated);
    } else if (!ok) {
        status = out.too_long ? K5_ENGINE_STATUS_TOO_LONG : K5_ENGINE_STATUS_FAILED;
        th_note(K5_NOTE_ENGINE_FAILED, fnv(out.why));
    }

    const std::string &body = status == K5_ENGINE_STATUS_OK ? out.answer : out.why;
    const size_t width = std::min<size_t>(body.size(), K5_ANSWER_MAX);
    if (status != K5_ENGINE_STATUS_CANCELLED && width > 0
        && th_memory_write(lent, 0, body.data(), width) != static_cast<int64_t>(width)) {
        status = caller_gone(invocation) ? K5_ENGINE_STATUS_CANCELLED : K5_ENGINE_STATUS_NO_BUFFER;
    }

    if (bound == THALYX_STATUS_OK) {
        th_unbind_worker(invocation);
    }

    reply.prompt_tokens = out.prompt_tokens;
    reply.generated = out.generated;
    reply.answer_len = static_cast<uint32_t>(width);
    reply.first_token = static_cast<uint32_t>(out.first_token);
    reply.first_margin_ppm = out.first_margin_ppm;
    reply.token_digest = out.token_digest;
    if (status == K5_ENGINE_STATUS_OK) {
        r.served++;
        reply.served = r.served;
        th_note(K5_NOTE_ENGINE_SERVED, r.served);
        th_note(K5_NOTE_ENGINE_TOKEN, static_cast<uint32_t>(out.first_token));
        th_note(K5_NOTE_ENGINE_ARGMAX, static_cast<uint32_t>(out.first_argmax));
        th_note(K5_NOTE_ENGINE_MARGIN, out.first_margin_ppm);
        th_note(K5_NOTE_ENGINE_DIGEST, out.token_digest);
        th_note(K5_NOTE_ENGINE_ELAPSED, reply.elapsed_ns);
    }
    finish(status);
}

} // namespace

extern "C" int th_main(const th_config *config)
{
    llama_log_set(log_sink, nullptr);

    Resident r;
    const uint32_t n_ctx = config->arg0 ? static_cast<uint32_t>(config->arg0) : 512;
    const int32_t n_threads = config->arg1 ? static_cast<int32_t>(config->arg1) : 1;

    // What the engine was given, named before anything reads it: the host
    // pinned these bytes, and the digest lets the weights this engine loaded
    // be matched to the host's copy without trusting anyone's say-so.
    th_note(K5_NOTE_ENGINE_MODEL_BYTES, config->bulk_bytes);
    th_note(K5_NOTE_ENGINE_MODEL_DIGEST,
            fnv(reinterpret_cast<const void *>(static_cast<uintptr_t>(TH_BULK_VADDR)),
                static_cast<size_t>(config->bulk_bytes)));

    const uint64_t started = th_now_ns();
    llama_backend_init();

    // Defaults, as Thalyx loads them. `use_mmap` stays on and llama.cpp finds
    // out for itself that this platform has no mapped files.
    const llama_model_params mp = llama_model_default_params();
    r.model = llama_model_load_from_file(TH_BULK_PATH, mp);
    if (!r.model) {
        th_note(K5_NOTE_ENGINE_LOAD_FAILED, fnv(last_warning, std::strlen(last_warning)));
        th_note(K5_NOTE_ENGINE_LOG_LINES, log_lines);
        return 2;
    }

    llama_context_params cp = llama_context_default_params();
    cp.n_ctx = n_ctx;
    cp.n_batch = static_cast<uint32_t>(r.n_batch);
    cp.n_threads = n_threads;
    cp.n_threads_batch = n_threads;
    r.ctx = llama_init_from_model(r.model, cp);
    if (!r.ctx) {
        th_note(K5_NOTE_ENGINE_LOAD_FAILED, fnv(last_warning, std::strlen(last_warning)));
        llama_model_free(r.model);
        return 3;
    }

    r.load_ns = th_now_ns() - started;
    r.weight_bytes = llama_model_size(r.model);
    th_note(K5_NOTE_ENGINE_LOADED, r.load_ns);
    th_note(K5_NOTE_ENGINE_WEIGHTS, r.weight_bytes);
    th_note(K5_NOTE_ENGINE_CONTEXT, llama_n_ctx(r.ctx));
    // What the profile declares about this engine, asked of llama.cpp itself:
    // on this platform there are no mapped files, and the weights this engine
    // holds are pages it read into memory charged to its own scope.
    th_note(K5_NOTE_ENGINE_MMAP, llama_supports_mmap() ? 1 : 0);
    th_note(K5_NOTE_ENGINE_LOG_LINES, log_lines);
    th_signal_raise(SLOT_READY, READY_BIT);

    const uint64_t endpoint = thalyx_boot_handle_of(TH_SLOT_INBOUND);
    for (;;) {
        th_message message;
        const int64_t got = th_receive(endpoint, &message, 0);
        if (got != THALYX_STATUS_OK) {
            // The endpoint is gone or this domain is being stopped: there is
            // nobody left to answer.
            if (got == THALYX_STATUS_PEER_DEAD || got == THALYX_STATUS_SCOPE_CLOSED) {
                break;
            }
            th_yield_ns(1000000);
            continue;
        }
        serve_request(r, message);
    }

    llama_free(r.ctx);
    llama_model_free(r.model);
    llama_backend_free();
    return 0;
}
