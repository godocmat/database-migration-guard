# Saved report formats

Render a saved version 1 Guard analysis report with `dmg report`. The render step reads local JSON and returns 0 once it writes the requested review. Use the original `dmg check` exit code and JSON decision for enforcement.

```sh
dmg report --input report.json --format html --output report.html
dmg report --input report.json --format sarif --output report.sarif
dmg report --input report.json --format gitlab --output gl-code-quality-report.json
dmg report --input report.json --format markdown --output summary.md
dmg report --input report.json --format github
```

`--output` replaces the destination atomically. Omit it to write to stdout. An unsupported report version, invalid JSON, or an invalid machine review source location returns 2. An unreadable input or output failure returns 3. Keep the JSON as the complete record for every review format.

## HTML

Open the standalone report in a browser. It shows the saved decision and coverage, findings, lock predictions, and any recorded policy audit. Findings expand to show the explanation, impact, mitigation, and bounded evidence. The page uses local system fonts, static CSS, a restrictive content security policy, and escaped report text. It loads no resources. The review shows up to 200 findings in severity order and states how many are omitted. Coverage gaps and lock predictions each show up to 50 entries, and gate reasons, baseline matches, and approvals each show up to 30.

## SARIF 2.1.0

SARIF includes one result per finding and the original Guard fingerprint in `fingerprints["dmgFinding/v1"]`. Critical and High map to `error`, Medium maps to `warning`, and Low and Info map to `note`. The original severity is also available as `properties.dmgSeverity` on each result. Locations use percent-encoded repository-relative URIs under `%SRCROOT%`, one-based Unicode scalar columns, and the saved half-open range. Resolve `%SRCROOT%` against the scanned checkout in the consuming tool.

An incomplete analysis is recorded in the run properties and invocation status. Up to 50 coverage gaps appear as tool execution notifications. The saved policy status and any recorded rehearsal outcome are available as run properties. The export uses the Guard finding fingerprint, whose inputs include the original byte range. SARIF consumers can apply their own matching and display rules. [SARIF specification](https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/sarif-v2.1.0-errata01-os-complete.html)

## GitLab Code Quality

Publish `gl-code-quality-report.json` as an `artifacts:reports:codequality` artifact. The file is a UTF-8 JSON array of findings with rule IDs, Guard fingerprints, original repository-relative paths, and beginning lines. Info, Low, Medium, High, and Critical map respectively to `info`, `minor`, `major`, `critical`, and `blocker`. Retain the Guard JSON and a human review artifact alongside it for coverage and gate status. [GitLab Code Quality report format](https://docs.gitlab.com/ci/testing/code_quality/#code-quality-report-format)

SARIF and GitLab Code Quality stream all findings and cap each message field. Both formats require validated relative finding paths of at most 1,024 UTF-8 bytes without control characters.

## Local verification

The Windows x86-64 CLI and renderer tests verify escaped report content, Unicode locations, unchanged fingerprints, omission counts, and preservation of existing output files when a review cannot be written. The OASIS schema validated four SARIF outputs covering denied, incomplete, empty, and untrusted saved report text. A strict HTML parser checked the corresponding standalone pages and a local validator checked GitLab's documented Code Quality fields. [OASIS SARIF 2.1.0 JSON schema](https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json)
