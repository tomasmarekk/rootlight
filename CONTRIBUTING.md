# Contributing to Rootlight

Thank you for your interest in Rootlight. This guide covers the project's
commit conventions.

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

## Development workflow

Delivery is trunk-based: small, scoped commits on `main`, then push and watch
the required CI jobs to completion. Keep changes focused; do not mix unrelated
work into one commit.
