// The surface a program sees, and nothing else.
//
// Ported semantics, not ported source: this is written here, and what it keeps
// from the surface being ported is the shape of the thing a model already
// writes against. `thalyx.call` is the one door; the named helpers are one line
// each over it, so a verb reached through a helper and a verb reached directly
// are the same call to the same machine.
//
// Two rules are enforced on the other side of that door and are only described
// here. A failed assertion latches: it is recorded where the program cannot
// reach it, it throws, and from that moment every host call refuses -- so a
// program cannot `try`/`catch` its way past the thing that was supposed to stop
// it, because the thing that stops it is not in the language. And a refusal is
// a value with `ok: false` in it rather than an error, because a program that
// could not write `if` would be back to asking a model about every mistake.
(function () {
  "use strict";
  const thalyx = globalThis.thalyx;

  function word(value, where) {
    if (typeof value !== "string") {
      throw new TypeError("thalyx." + where + ": the verb has to be a string");
    }
    return value;
  }

  function words(values, where) {
    if (!Array.isArray(values)) {
      throw new TypeError("thalyx." + where + ": the arguments have to be an array");
    }
    return values.map(function (value) {
      if (typeof value !== "string") {
        throw new TypeError(
          "thalyx." + where + ": every argument must be a string, and one is " + typeof value
        );
      }
      return value;
    });
  }

  thalyx.call = function (verb, args) {
    return thalyx.__call(word(verb, "call"), words(args === undefined ? [] : args, "call"));
  };

  thalyx.state = () => thalyx.call("estado", []);
  thalyx.read = (name) => thalyx.call("leer", [name]);
  thalyx.list = () => thalyx.call("listar", []);
  thalyx.substitute = (name, before, after) =>
    thalyx.call("sustituir", [name, before, after]);
  thalyx.write = (name, text) => thalyx.call("escribir", [name, text]);
  thalyx.grep = (text) => thalyx.call("buscar", [text]);
  thalyx.context = (name) => thalyx.call("contexto", [name]);
  thalyx.changed = () => thalyx.__changed();
  thalyx.validate = (check) => thalyx.__validate(JSON.stringify(check === undefined ? {} : check));
  thalyx.model = (prompt, predict) => thalyx.__model(String(prompt), predict === undefined ? 16 : predict);
  thalyx.log = (text) => thalyx.__log(String(text));

  thalyx.assert = function (held, what, detail) {
    if (!held) {
      thalyx.__assert(String(what === undefined ? "an assertion did not hold" : what),
                      JSON.stringify(detail === undefined ? null : detail));
      throw new Error("assertion: " + String(what));
    }
    return true;
  };

  // The same for an answer, whose "did it work" is `ok`.
  thalyx.mustWork = function (answer, what) {
    thalyx.assert(answer && answer.ok === true, what, answer);
    return answer;
  };

  // And for a validation, whose "did it work" is a verdict and not `ok`. Three
  // outcomes and not two: `not_proven` is neither, and a program that treated
  // it as a pass would commit over a check that never ran.
  thalyx.mustPass = function (record, what) {
    thalyx.assert(
      record && record.verdict === "passed",
      what === undefined ? "a check did not hold" : what,
      record
    );
    return record;
  };

  // The program decided the next decision is not one a machine should make.
  thalyx.needsModel = function (value) {
    thalyx.__needs_model(JSON.stringify(value === undefined ? null : value));
    throw new Error("needs_model");
  };

  Object.freeze(thalyx);
})();
