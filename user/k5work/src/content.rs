//! The content the vertical is about.
//!
//! A port needs something to work on. This is it: a small JavaScript module,
//! its assertions, and the program a model would have written to change it. In
//! Thalyx the first two live in a repository and the third arrives from an
//! inference; here the first two are the seed version this run publishes before
//! it does any work, and the third is a module of the package. The evidence
//! says so rather than implying that a model wrote it.
//!
//! The mark is what makes the vertical falsifiable. The seed version carries
//! sixteen zeroes; the work replaces them with the run's own seed in hex; the
//! module's own assertions fail while the mark is zeroes and hold once it is
//! not. So "the check ran" and "the change happened" are two observations that
//! can disagree, and the host can recompute what the published bytes must be
//! from a seed the guest was given rather than chose.

/// The name of the module under work.
pub const NAME_MODULE: &[u8] = b"module.js";

/// The name of the program the language runtime executes.
pub const NAME_PROGRAM: &[u8] = b"program.js";

/// The mark the seed version carries and the work replaces.
///
/// Both works of the `rivals` scenario replace this same mark, each with a
/// mark of its own. That is what makes them rivals rather than two works: the
/// module's own tests hold only once the mark is gone, so neither can leave it
/// to the other, and the one whose publication is refused starts again over
/// the version that won, finds the placeholder already replaced, and has
/// nothing it can honestly do but abandon -- with the evidence of the attempt
/// published nowhere and recorded everywhere.
pub const ZERO_MARK: &[u8] = b"0000000000000000";

/// The name the publisher records what the model said under.
pub const NAME_MODEL_RECORD: &[u8] = b"model.json";

/// The name the rival records what the model said under.
pub const NAME_RIVAL_RECORD: &[u8] = b"rival.json";

/// Bytes of a mark.
pub const MARK_LEN: usize = 16;

/// The module under work, as the seed version publishes it.
pub const MODULE: &[u8] = br#""use strict";
// The module this run's work is about. Its mark starts as zeroes and the
// work replaces it with the seed of the run; the assertions in
// module.test.js hold only once that has happened.
function checksum(text) {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i) & 0xff;
    h = (Math.imul(h, 16777619)) >>> 0;
  }
  return h >>> 0;
}
var MARK = "0000000000000000";
function mark() { return MARK; }
// Written against a mark it builds rather than one it quotes, so the literal
// sixteen zeroes appear exactly once in this file. `sustituir` replaces one
// occurrence on purpose -- a substitution that quietly changed every match
// would be the edit nobody reviews -- and a second literal would leave the
// module half marked and the work would be right to refuse it.
function marked() { return MARK !== "0".repeat(16); }
"#;

/// The assertions a validation tool runs over the module.
///
/// `check` is provided by whatever runs this; the tool binds it and counts.
/// Nothing here reaches outside itself, which is the point: a check that could
/// reach the system would be a check that could pass for the wrong reason.
pub const TESTS: &[u8] = br#""use strict";
check(typeof checksum === "function", "checksum is defined");
check(checksum("") === 2166136261, "the empty string hashes to the offset basis");
check(checksum("ab") !== checksum("ba"), "the hash depends on order");
check(checksum("thalyx") === checksum("thalyx"), "the hash is a function");
check(mark().length === 16, "the mark is sixteen characters");
check(/^[0-9a-f]{16}$/.test(mark()), "the mark is lower-case hexadecimal");
check(marked(), "the mark is no longer the seed version's zeroes");
check(checksum(mark()) !== 0, "the mark hashes to something");
"#;

/// The program the work executes in the language runtime.
///
/// It is JavaScript because that is what the surface being ported executes, and
/// every authority it has is a call: there is no filesystem in it, no network,
/// no process and no clock that reaches outside. What it can reach is exactly
/// the union of what its calls could have reached one at a time.
pub const PROGRAM: &[u8] = br#""use strict";
const state = thalyx.mustWork(thalyx.call("estado", []), "read the published version");
const before = thalyx.mustWork(thalyx.call("leer", ["module.js"]), "read the module");
thalyx.assert(before.text.indexOf(state.zero_mark) >= 0, "the seed version is unmarked");

const context = thalyx.mustWork(thalyx.call("contexto", ["checksum"]), "ask what checksum is");
thalyx.assert(context.uses >= 2, "checksum is used more than once", context);

thalyx.log("marking with " + state.mark);
thalyx.mustWork(
  thalyx.call("sustituir", ["module.js", state.zero_mark, state.mark]),
  "replace the mark"
);
const after = thalyx.mustWork(thalyx.call("leer", ["module.js"]), "read it back");
thalyx.assert(after.text.indexOf(state.mark) >= 0, "the mark is in the module");
thalyx.assert(after.text.indexOf(state.zero_mark) < 0, "the old mark is gone");

const changed = thalyx.changed();
thalyx.assert(changed.count === 1, "exactly one name changed", changed);

let sum = 2166136261;
for (let i = 0; i < after.text.length; i++) {
  sum ^= after.text.charCodeAt(i) & 0xff;
  sum = (Math.imul(sum, 16777619)) >>> 0;
}

const verdict = thalyx.mustPass(thalyx.validate({ check: "program" }), "the tool passed");
return { mark: state.mark, changed: changed.count, checks: verdict.checks_run, sum: sum >>> 0 };
"#;

/// The program when a resident engine is there to ask.
///
/// The same change as [`PROGRAM`] -- read the version, ask what a name is,
/// mark a file, check the change, have a real tool decide -- and before
/// validating it asks the model and puts what the model said into the version
/// it publishes. Two answers from one engine are how residency is shown rather
/// than claimed: the second one says it was the second, and that the weights
/// were loaded once.
///
/// What it edits, what it records under, and what it asks the model come from
/// `estado`: the work's own intent, which the program reads rather than
/// carries. The publisher and the rival run this same program over different
/// intents, which is what makes them rivals rather than two programs.
pub const PROGRAM_ENGINE: &[u8] = br#""use strict";
const state = thalyx.mustWork(thalyx.call("estado", []), "read the published version");
const before = thalyx.mustWork(thalyx.call("leer", [state.target]), "read the target");
thalyx.assert(before.text.indexOf(state.placeholder) >= 0, "the target still has its placeholder");

const context = thalyx.mustWork(thalyx.call("contexto", ["checksum"]), "ask what checksum is");
thalyx.assert(context.uses >= 2, "checksum is used more than once", context);

thalyx.mustWork(
  thalyx.call("sustituir", [state.target, state.placeholder, state.mark]),
  "replace the placeholder"
);
const after = thalyx.mustWork(thalyx.call("leer", [state.target]), "read it back");
thalyx.assert(after.text.indexOf(state.mark) >= 0, "the mark is in the target");
thalyx.assert(after.text.indexOf(state.placeholder) < 0, "the placeholder is gone");

const prompts = state.prompts;
const first = thalyx.model(prompts[0], state.predict);
thalyx.assert(first.ok === true, "the engine answered", first);
const second = thalyx.model(prompts[1], state.predict);
thalyx.assert(second.ok === true, "the engine answered again", second);
thalyx.assert(second.served > first.served, "the same engine served both, in order", second);
thalyx.assert(second.load_ns === first.load_ns, "and loaded its weights once", second);

const answers = [first, second].map((a, i) => ({
  prompt: prompts[i],
  prompt_digest: a.prompt_digest,
  text_hex: a.text_hex,
  generated: a.generated,
  first_token: a.first_token,
  first_margin_ppm: a.first_margin_ppm,
  token_digest: a.token_digest,
  served: a.served,
}));
thalyx.mustWork(
  thalyx.call("escribir", [state.record, JSON.stringify({ engine: "llama.cpp b10665", answers: answers })]),
  "record what the model said"
);

const changed = thalyx.changed();
thalyx.assert(changed.count === 2, "the target and the record changed", changed);

const verdict = thalyx.mustPass(thalyx.validate({ check: "program" }), "the tool passed");
return { mark: state.mark, changed: changed.count, checks: verdict.checks_run, served: second.served };
"#;

/// Prose that travels with the version, so a version is not only code.
pub const NOTES: &[u8] = br#"# module.js

The mark in this module is the seed of the run that published it. A version
whose mark is still sixteen zeroes has not been worked on.
"#;

/// Writes `value` as sixteen lower-case hexadecimal digits.
pub fn mark_of(value: u64, out: &mut [u8; MARK_LEN]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for index in 0..MARK_LEN {
        let shift = 60 - index * 4;
        out[index] = HEX[((value >> shift) & 0xF) as usize];
    }
}
