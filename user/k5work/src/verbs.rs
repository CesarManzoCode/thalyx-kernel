//! The Thalyx verb surface, answered here.
//!
//! One implementation, two callers. The `surface` stage drives it from a script
//! the host wrote outside the guest; the `work` stage drives it from a program
//! the language runtime executes. That is deliberate and it is the property
//! `vault/integration/thalyx.md` asks for in the row about `hacer`: a program
//! is not a second way to reach a verb, and what a program can reach is exactly
//! the union of what its calls could have reached one at a time. If the two
//! callers went through different code, that sentence would be a hope.
//!
//! Every answer is JSON, because the language runtime parses it with its own
//! `JSON.parse` rather than with a format either side invented. Every answer
//! carries `ok`, and a refusal is a value the caller branches on rather than an
//! error that ends the run: a program that could not write `if` would be back to
//! asking a model what to do about every mistake it makes.

use thalyx_user_k5pkg::thalyx::{Json, verb};
use thalyx_user_rt::k2;

use crate::content;
use crate::tree::{self, Workspace};

/// What a verb call needs besides its arguments.
pub struct Context<'a> {
    /// The published generation this work is against.
    pub generation: u64,
    /// The root digest of that version.
    pub root: [u8; 32],
    /// The run's seed, and the mark derived from it.
    pub seed: u64,
    /// Whether the workspace may still be changed.
    pub open: bool,
    /// The workspace.
    pub workspace: &'a mut Workspace,
}

/// One argument of a verb call.
pub type Arg<'a> = &'a [u8];

/// Answers one verb call into `out`, returning the bytes written.
///
/// A verb this surface does not have is a refusal that names it, not a panic
/// and not a silence: the caller is untrusted and asking for something that is
/// not there is an ordinary thing for it to do.
pub fn answer(context: &mut Context<'_>, name: Arg<'_>, args: &[Arg<'_>], out: &mut [u8]) -> usize {
    let Some(code) = core::str::from_utf8(name).ok().and_then(verb::of) else {
        return refuse(out, b"no_such_verb", name);
    };
    match code {
        verb::STATE => state(context, out),
        verb::READ => read(context, args, out),
        verb::LIST => list(context, out),
        verb::SUBSTITUTE => substitute(context, args, out),
        verb::WRITE => write(context, args, out),
        verb::CHANGED => changed(context, out),
        verb::GREP => grep(context, args, out),
        verb::CONTEXT => context_of(context, args, out),
        _ => refuse(out, b"no_such_verb", name),
    }
}

fn refuse(out: &mut [u8], word: &[u8], detail: &[u8]) -> usize {
    let mut json = Json::new(out);
    json.open();
    json.field_bool("ok", false);
    json.field_string("error", word);
    json.field_string("detail", detail);
    json.close();
    json.finish().unwrap_or(0)
}

fn state(context: &mut Context<'_>, out: &mut [u8]) -> usize {
    let mut mark = [0u8; content::MARK_LEN];
    content::mark_of(context.seed, &mut mark);
    let mut json = Json::new(out);
    json.open();
    json.field_bool("ok", true);
    json.field_number("generation", context.generation);
    json.key("root");
    json.digest(&context.root);
    json.field_string("mark", &mark);
    json.field_string("zero_mark", content::ZERO_MARK);
    json.field_number("names", context.workspace.len() as u64);
    json.field_bool("open", context.open);
    json.close();
    json.finish().unwrap_or(0)
}

fn read(context: &mut Context<'_>, args: &[Arg<'_>], out: &mut [u8]) -> usize {
    let Some(name) = args.first() else {
        return refuse(out, b"bad_argument", b"leer needs a name");
    };
    let Some(bytes) = context.workspace.read(name) else {
        return refuse(out, b"no_such_name", name);
    };
    let mut json = Json::new(out);
    json.open();
    json.field_bool("ok", true);
    json.field_string("name", name);
    json.field_number("bytes", bytes.len() as u64);
    json.field_string("text", bytes);
    json.close();
    match json.finish() {
        Some(written) => written,
        // Refused rather than truncated: an answer cut in half is an answer a
        // program will act on believing it is whole.
        None => refuse(out, b"too_large", name),
    }
}

fn list(context: &mut Context<'_>, out: &mut [u8]) -> usize {
    let mut json = Json::new(out);
    json.open();
    json.field_bool("ok", true);
    json.key("names");
    json.open_array();
    for entry in context.workspace.entries() {
        json.open();
        json.field_string("name", entry.name());
        json.field_number("bytes", entry.bytes().len() as u64);
        json.field_bool("changed", entry.changed);
        json.close();
    }
    json.close_array();
    json.close();
    json.finish().unwrap_or(0)
}

fn substitute(context: &mut Context<'_>, args: &[Arg<'_>], out: &mut [u8]) -> usize {
    if !context.open {
        return refuse(out, b"not_open", b"the workspace is frozen");
    }
    let (Some(name), Some(before), Some(after)) = (args.first(), args.get(1), args.get(2)) else {
        return refuse(
            out,
            b"bad_argument",
            b"sustituir needs a name, a before and an after",
        );
    };
    match context.workspace.substitute(name, before, after) {
        Ok(at) => {
            let mut json = Json::new(out);
            json.open();
            json.field_bool("ok", true);
            json.field_string("name", name);
            json.field_number("at", at as u64);
            json.field_number("changed", context.workspace.changed() as u64);
            json.close();
            json.finish().unwrap_or(0)
        }
        Err(error) => refuse(out, error.word().as_bytes(), name),
    }
}

fn write(context: &mut Context<'_>, args: &[Arg<'_>], out: &mut [u8]) -> usize {
    if !context.open {
        return refuse(out, b"not_open", b"the workspace is frozen");
    }
    let (Some(name), Some(text)) = (args.first(), args.get(1)) else {
        return refuse(out, b"bad_argument", b"escribir needs a name and a text");
    };
    match context.workspace.put(name, text, false) {
        Ok(()) => {
            let mut json = Json::new(out);
            json.open();
            json.field_bool("ok", true);
            json.field_string("name", name);
            json.field_number("bytes", text.len() as u64);
            json.field_number("changed", context.workspace.changed() as u64);
            json.close();
            json.finish().unwrap_or(0)
        }
        Err(error) => refuse(out, error.word().as_bytes(), name),
    }
}

/// What the workspace really shows changed, observed and not remembered.
///
/// What a call *said* it changed is a claim by the call. This compares content
/// against the version the workspace was forked from, which is the only thing
/// that can contradict the claim.
fn changed(context: &mut Context<'_>, out: &mut [u8]) -> usize {
    let mut json = Json::new(out);
    json.open();
    json.field_bool("ok", true);
    json.field_number("count", context.workspace.changed() as u64);
    json.key("names");
    json.open_array();
    for entry in context.workspace.entries() {
        if entry.changed {
            json.string(entry.name());
        }
    }
    json.close_array();
    json.close();
    json.finish().unwrap_or(0)
}

fn grep(context: &mut Context<'_>, args: &[Arg<'_>], out: &mut [u8]) -> usize {
    let Some(needle) = args.first() else {
        return refuse(out, b"bad_argument", b"buscar needs a text");
    };
    let mut json = Json::new(out);
    json.open();
    json.field_bool("ok", true);
    json.field_string("text", needle);
    json.key("hits");
    json.open_array();
    let mut total = 0usize;
    for entry in context.workspace.entries() {
        let found = tree::count_bytes(entry.bytes(), needle);
        if found == 0 {
            continue;
        }
        total += found;
        json.open();
        json.field_string("name", entry.name());
        json.field_number("count", found as u64);
        json.close();
    }
    json.close_array();
    json.field_number("total", total as u64);
    json.close();
    json.finish().unwrap_or(0)
}

/// `contexto`: what a name is, small enough to read.
///
/// Thalyx answers this from an index a compiler frontend built. There is no
/// such frontend here and none is faked. What this does is count occurrences
/// across the version the work is against and say so, including the word for
/// what it did not do: the coverage it claims is `textual`, and a caller that
/// needed a resolved symbol is told it did not get one.
fn context_of(context: &mut Context<'_>, args: &[Arg<'_>], out: &mut [u8]) -> usize {
    let Some(name) = args.first() else {
        return refuse(out, b"bad_argument", b"contexto needs a name");
    };
    let mut uses = 0usize;
    let mut defined_in: Option<&[u8]> = None;
    let mut definition = [0u8; 96];
    let mut definition_len = 0usize;
    for entry in context.workspace.entries() {
        let found = tree::count_bytes(entry.bytes(), name);
        uses += found;
        if defined_in.is_none() {
            let mut pattern = [0u8; 48];
            let head = b"function ";
            if head.len() + name.len() <= pattern.len() {
                pattern[..head.len()].copy_from_slice(head);
                pattern[head.len()..head.len() + name.len()].copy_from_slice(name);
                let width = head.len() + name.len();
                if let Some(at) = tree::find_bytes(entry.bytes(), &pattern[..width]) {
                    defined_in = Some(entry.name());
                    let line = &entry.bytes()[at..];
                    let end = tree::find_bytes(line, b"\n").unwrap_or(line.len()).min(96);
                    definition[..end].copy_from_slice(&line[..end]);
                    definition_len = end;
                }
            }
        }
    }
    let mut json = Json::new(out);
    json.open();
    json.field_bool("ok", true);
    json.field_string("name", name);
    json.field_number("uses", uses as u64);
    json.field_string("defined_in", defined_in.unwrap_or(b""));
    json.field_string("signature", &definition[..definition_len]);
    // What this answer is, said in the answer. A caller that needed a name
    // resolved by a compiler frontend has been told it did not get one.
    json.field_string("coverage", b"textual");
    json.field_bool("resolved", false);
    json.close();
    json.finish().unwrap_or(0)
}

/// Reports one verb call on the diagnostic plane, so a gate can count them.
pub fn note_call(name: Arg<'_>) {
    let mut packed = 0u64;
    for byte in name.iter().take(8) {
        packed = (packed << 8) | u64::from(*byte);
    }
    k2::note(thalyx_user_k5pkg::proto::note::HOSTCALL, packed);
}
