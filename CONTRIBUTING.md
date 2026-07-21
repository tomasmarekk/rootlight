# Contributing to Rootlight

Thank you for your interest in Rootlight. This guide covers the commit
conventions enforced by CI and the local hook.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <imperative summary>
```

- Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`.
- Keep the subject concise, lowercase, under 50 characters when possible, with
  no trailing period.
- Describe what changed and why, not how. Add a body only when needed for
  rationale, breaking changes, or migration notes.

Compliant examples:

```
fix(auth): handle expired tokens
feat(mcp): paginate repo.list with authenticated cursors
refactor(query): split planning from execution
docs: clarify cursor expiry behavior
```

Commit messages must describe product behavior. Do not put internal planning,
tracking, phase, gate, or requirement identifiers in commit subjects or bodies;
CI rejects them. Identifiers belong in private planning material, never in Git
metadata, public source, schemas, fixtures, or release artifacts.

## Local commit hook

Install the commit-message hook once per clone so violations are caught before
they reach CI:

```
git config core.hooksPath .githooks
```

## Development workflow

Delivery is trunk-based: small, scoped commits on `main`, then push and watch
the required CI jobs to completion. Keep changes focused; do not mix unrelated
work into one commit.
