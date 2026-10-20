# Tilewright world repository guide

- This repository owns the game-neutral world format specification
  (`docs/spec/`), the toolkit crates that read, validate, edit and compile it,
  and the CLI. It contains no game-specific code; games supply profiles.
- The specification and its fixtures change together. A format change updates
  `docs/spec/`, the parser/model, fixtures and tests in one pull request.
- Parsing is strict, bounded and fail-closed: enforce the limits in
  `docs/spec/FORMAT.md` while reading, never after building an unbounded tree.
  Untrusted input must never panic, allocate without bound or execute code.
- Editing is lossless: preserve comments, ordering, whitespace and untouched
  bytes; edits replace exact byte spans and are re-validated.
- Compilation is deterministic: identical source and compiler version produce
  byte-identical packs.
- New code, tests, docs and fixtures are MIT. Do not copy, adapt or depend on
  GPL/AGPL source. Record any reuse of outside MIT-compatible material in
  `PROVENANCE.md`.
- `fixtures/packs` is generated: when compiler output changes, regenerate
  it as its README says, and keep the Rust and Go readers' error codes
  equal to the manifest's.
- Run `tools/validate.sh` and `git diff --check`. Commits and PR titles use
  Conventional Commits. Merges to `main` are released, versioned by their
  titles (`docs/RELEASING.md`).
