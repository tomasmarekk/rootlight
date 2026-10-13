# CodeQL coverage

The required `CodeQL` workflow analyzes Rust and JavaScript/TypeScript in
parallel on every push to `main` and every pull request. Both languages use the
complete pinned `security-extended` suite, including `js/file-system-race`.
The official Init and Analyze actions own extraction and query evaluation;
there are no database copies, query partitions, or custom SARIF merges.

`source.yml` retains all maintained source, tests, tooling, packaging, grammar
bindings, and locally maintained producer security patches. It excludes the
original upstream Cargo, producer, and producer-dependency inventories plus
generated build outputs. These exclusions reduce the routine static-analysis
scope; they do not classify active dependencies as unused or prove their safety.
The dependency, advisory, provenance, and source-integrity CI gates still apply.

`CodeQL Full` independently scans the complete repository each week and can be
started manually. It uses the same authenticated scanners and full query suites,
without source-path exclusions. Its separate analysis categories preserve full
source findings when routine scans run, and its independent concurrency group
allows a scheduled scan to finish while development continues.

Pull requests have read-only repository permissions and retain analysis results
without a privileged upload. Main-branch analyses retain SARIF before upload and
require GitHub to finish processing it. `codeql-required` succeeds only when the
expected complete language matrix succeeds for the current event. Timeouts and
cancelled analyses remain failures; a shorter deadline is not proof of speed.

Both scanner archives are checksum-pinned in the setup scripts. Review their
versions and published query suites together when updating the tooling.
GitHub documents this separation of routine and scheduled complete analysis in
[its guidance for slow scans](https://docs.github.com/en/code-security/reference/code-scanning/troubleshoot-analysis-errors/analysis-takes-too-long).
