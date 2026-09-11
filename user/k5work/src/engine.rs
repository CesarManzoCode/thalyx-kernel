//! Asking the resident engine, from inside a program's run.
//!
//! The engine is a service domain, not a library: the weights are loaded once,
//! in a scope of its own, and every inference is an invocation. That shape is
//! `vault/integration/thalyx.md`'s row about the resident engine -- *dominio de
//! servicio, pesos patrocinados y tickets por inferencia* -- and the two
//! properties it exists to keep are the ones this file is careful about.
//!
//! **Authority is per request.** The prompt travels in a buffer this work owns
//! and lends with the call, narrowed to read for the prompt and write for the
//! answer. Two works using the same engine lend different buffers, so neither
//! can read the other's prompt or answer, and neither holds a capability the
//! other does.
//!
//! **Cost is attributed.** The engine binds a worker to the invocation, so the
//! kernel charges the inference to the scope of the work that asked for it,
//! while the weights stay charged to the engine's own. What a work spent on
//! inference and what residency costs are then two numbers the kernel produced,
//! not two numbers a program claimed.

use thalyx_abi::{cap_op, right, status};
use thalyx_user_k4fmt::Pod;
use thalyx_user_k5pkg::proto::{
    EngineReply, EngineRequest, engine_op, engine_status, note, work_addr,
};
use thalyx_user_k5pkg::thalyx::Json;
use thalyx_user_rt::k2;

use crate::hacer::Driver;

/// How long an inference may take before the caller stops waiting.
const INFER_DEADLINE_NS: u64 = 120_000_000_000;

/// The buffer this work lends the engine, and lends nobody else.
///
/// # Safety
///
/// The supervisor maps `PROMPT_PAGES` writable pages there before activating
/// this domain, and maps them nowhere else in it.
fn prompt_buffer() -> &'static mut [u8] {
    // SAFETY: as the doc comment says.
    unsafe {
        core::slice::from_raw_parts_mut(
            work_addr::PROMPT as *mut u8,
            (work_addr::PROMPT_PAGES as usize) * 4096,
        )
    }
}

/// FNV-1a over bytes: the digest the engine reports over the prompt it read,
/// computed here over the prompt this work lent, so the two can be compared.
fn fnv(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Answers one `thalyx.model` call, into `out`.
pub fn answer(driver: &mut Driver<'_>, prompt: &[u8], predict: u64, out: &mut [u8]) -> usize {
    if driver.engine == 0 {
        let mut json = Json::new(out);
        json.open();
        json.field_bool("ok", false);
        json.field_string("error", b"no_engine");
        // Said rather than implied: this role was built without a facet of the
        // engine, so the refusal is a fact about its capability table.
        json.field_string("detail", b"this work holds no engine capability");
        json.close();
        return json.finish().unwrap_or(0);
    }

    let width = prompt.len().min(thalyx_user_k5pkg::proto::PROMPT_MAX);
    prompt_buffer()[..width].copy_from_slice(&prompt[..width]);
    let prompt_digest = fnv(&prompt[..width]);
    k2::note(note::ENGINE_PROMPT, prompt_digest);

    let request = EngineRequest {
        op: engine_op::INFER,
        predict: predict.min(thalyx_user_k5pkg::proto::engine_case::CONTEXT_TOKENS) as u32,
        prompt_len: width as u32,
        grammar_len: 0,
        seed: driver.seed,
    };
    let lent = match k2::derive(
        driver.prompt,
        right::INSPECT | right::TRANSFER | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
        0,
    ) {
        Ok(handle) => handle,
        Err(_) => {
            let mut json = Json::new(out);
            json.open();
            json.field_bool("ok", false);
            json.field_string("error", b"no_buffer");
            json.close();
            return json.finish().unwrap_or(0);
        }
    };

    let answered = k2::endpoint_call(
        driver.engine,
        4,
        request.as_bytes(),
        &[(lent, cap_op::MOVE)],
        k2::now_ns() + INFER_DEADLINE_NS,
        false,
    );
    let refused = answered.as_ref().err().copied();
    let reply = answered
        .ok()
        .and_then(|result| EngineReply::read_from(&result.payload, 0));

    let mut json = Json::new(out);
    json.open();
    match reply {
        Some(reply) if reply.status == engine_status::OK => {
            let len = (reply.answer_len as usize).min(thalyx_user_k5pkg::proto::ANSWER_MAX);
            let completion = &prompt_buffer()[..len];
            json.field_bool("ok", true);
            json.field_latin1("text", completion);
            json.field_hex("text_hex", completion);
            json.field_number("prompt_tokens", u64::from(reply.prompt_tokens));
            json.field_number("generated", u64::from(reply.generated));
            json.field_number("first_token", u64::from(reply.first_token));
            json.field_number("first_margin_ppm", u64::from(reply.first_margin_ppm));
            // Digests as hexadecimal: a JavaScript number is a double and would
            // round them, and a rounded digest is a different digest.
            json.field_hex("token_digest", &reply.token_digest.to_be_bytes());
            json.field_hex("prompt_digest", &prompt_digest.to_be_bytes());
            json.field_number("elapsed_ns", reply.elapsed_ns);
            // Residency, in every answer, so it is a number a caller sees
            // rather than a claim somebody made once.
            json.field_number("load_ns", reply.load_ns);
            json.field_number("served", reply.served);
            json.field_number("weight_bytes", reply.weight_bytes);
            k2::note(note::ENGINE_SERVED, reply.served);
            k2::note(note::ENGINE_TOKEN, u64::from(reply.first_token));
            k2::note(note::ENGINE_MARGIN, u64::from(reply.first_margin_ppm));
            k2::note(note::ENGINE_DIGEST, reply.token_digest);
            k2::note(note::ENGINE_ELAPSED, reply.elapsed_ns);
        }
        Some(reply) => {
            json.field_bool("ok", false);
            json.field_string("error", b"engine_refused");
            json.field_number("status", u64::from(reply.status));
            k2::note(note::ENGINE_REFUSED, u64::from(reply.status));
        }
        // The call itself did not come back with an answer. `CANCELLED` is the
        // kernel saying this work's own scope was closed while the engine was
        // computing for it -- the one refusal that is about the caller and not
        // about the engine, and the program is told which it was.
        None if refused == Some(status::CANCELLED) => {
            json.field_bool("ok", false);
            json.field_string("error", b"cancelled");
            k2::note(
                note::ENGINE_REFUSED,
                (-status::CANCELLED) as u64 | (1 << 32),
            );
        }
        None => {
            json.field_bool("ok", false);
            json.field_string("error", b"engine_unreachable");
            json.field_number("status", refused.map_or(0, |code| (-code) as u64));
            k2::note(note::ENGINE_REFUSED, u64::MAX);
        }
    }
    json.close();
    json.finish().unwrap_or(0)
}
