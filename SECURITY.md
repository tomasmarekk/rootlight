# Security policy

## Reporting a vulnerability

Use a private GitHub security advisory for vulnerabilities that could expose
repository contents, escape an adapter sandbox, corrupt persistent state,
compromise release signing, or weaken the local-only trust boundary. Assign the
report to `@tomasmarekk`.

Do not attach repository source, credentials, access tokens, private keys,
unredacted environment dumps, or raw tool output. Prefer minimal synthetic
reproduction steps, stable diagnostic codes, artifact digests, and the
source-free support bundle produced by Rootlight.

Include:

- the affected Rootlight version and platform;
- whether the daemon, CLI, MCP bridge, or adapter host was involved;
- the observed stable error or health code;
- the smallest synthetic reproduction;
- any known evidence of source or secret exposure; and
- a safe way to coordinate follow-up.

Do not open a public issue until the incident owner confirms that disclosure is
safe.

## Response expectations

The incident owner acknowledges a credible private report, assigns a severity,
preserves redacted evidence, and follows
[the incident-response runbook](operations/INCIDENT_RESPONSE.md). Fix timing
depends on severity, exploitability, and the ability to ship a verified update.
Release publication stops whenever artifact signatures, provenance, SBOMs,
license status, or rollback evidence cannot be verified.

Rootlight does not request private repository contents to diagnose an incident.
If a reproduction cannot be made source-free, coordinate the minimum necessary
disclosure privately before transferring any data.
