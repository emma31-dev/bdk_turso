# Security policy

`bdk_turso` is a persistence backend, so vulnerabilities in its own code and in the dependency tree can affect wallets that store keys, descriptors, PSBTs, and transaction history.

## Supported versions

Security fixes are made against the latest published minor release and `main`. The crate is pre-1.0, so a fix may ship in the next minor release when a patch release cannot preserve the public API.

## Reporting a vulnerability

Report suspected vulnerabilities privately through the repository's GitHub Security Advisories page:

<https://github.com/emma31-dev/bdk_turso/security/advisories/new>

Do not open a public issue for an unpatched vulnerability. Include the affected version, a reproduction or proof of concept if available, the impact you believe is possible, and any suggested fix. You will receive an acknowledgement, and the report will be triaged before any public disclosure.

## Response targets

- Acknowledge a report within 3 business days.
- Confirm or reject the report within 10 business days.
- Publish a patched release for a confirmed reachable vulnerability as soon as a fix is available. Security releases may bypass the normal release cadence.
- Publish a GitHub Security Advisory after a fix ships, with credit unless the reporter asks to remain anonymous.

These targets are best-effort for a small project, not a contractual service level.

## Dependency and supply-chain policy

Turso pulls in a large transitive tree. The project treats that tree as part of its attack surface rather than assuming an upstream crate is automatically safe.

- CI runs `cargo-deny` on every push and pull request. The check covers RustSec advisories, licenses, wildcard and duplicate-version bans, and registry sources.
- A new vulnerability fails CI unless `deny.toml` contains an explicit exception. Every exception names the advisory, explains why the affected code is not reachable or why the fix cannot be applied yet, and sets an expiry date. Exceptions are reviewed before they expire.
- Licenses must be on the allow-list in `deny.toml`. A dependency that adds a new license fails CI until the license is reviewed and explicitly allowed.
- Dependencies must come from crates.io. Git and unknown registries are denied.
- `Cargo.lock` is committed, so releases build from the reviewed dependency graph.

## Triage process

For each advisory reported against a dependency:

1. Identify the affected crate, version range, and whether the advisory is a vulnerability, unsoundness, unmaintained package, or yanked release.
2. Determine whether the vulnerable code is reachable from `bdk_turso`. Record the call path or explain why it is unreachable.
3. Prefer upgrading the dependency, directly or through `cargo update`. For a fix constrained by an upstream version range, try a precise compatible update before considering an exception.
4. If an upgrade is not possible, add a time-limited `deny.toml` exception with a reason and expiry, and open a tracking issue.
5. Re-run the audit and the full test matrix before merging.

An advisory is closed only when no known reachable vulnerability remains in the shipped dependency tree.

## Audit baseline (2026-11-25)

The 0.4.0 dependency refresh moved `time` from 0.3.45 to 0.3.55, resolving RUSTSEC-2026-0009 (`CVE-2026-25727`, `GHSA-r6v5-fh4h-64xc`). The full locked graph was then checked against the RustSec advisory database. Three findings remain, each with a documented disposition in `deny.toml`:

| Advisory | Crate | Disposition |
| --- | --- | --- |
| RUSTSEC-2023-0071 | `rsa` 0.9.10 | Not shipped. Reachable only through `sqlx-mysql`, a dev-dependency of `bdk_sqlite` used by benchmarks and tests. `bdk_turso` does not link `rsa` or perform RSA operations. Exception expires 2027-02-28. |
| RUSTSEC-2026-0459 | `libsqlite3-sys` 0.30.1 | Not shipped. Reachable only through the `bdk_sqlite` dev-dependency. The published crate uses Turso, not bundled or system SQLite. Exception expires 2027-02-28. |
| RUSTSEC-2026-0543 | `chrono` 0.4.45 | Unmaintained transitive dependency of Turso. `bdk_turso` has no replacement it can apply directly and does not pass attacker-controlled RFC 3339 input through `chrono`. Re-evaluate on the next Turso upgrade; exception expires 2027-02-28. |

No yanked releases, unknown registries, or git sources were present at this baseline. Re-run the audit when any dependency is upgraded, and remove an exception as soon as the underlying finding no longer appears.

## Turso upgrades

Turso is pre-1.0 and changes quickly. When `turso` is upgraded:

1. Review the upstream changelog and security releases.
2. Run `cargo deny check` and review the updated `Cargo.lock`.
3. Run `cargo tree -d` to review duplicate versions introduced by the upgrade.
4. Run the full feature and MSRV test matrix.
5. Re-run the benchmark suite when the upgrade can affect query or transaction performance.

The same review applies when Turso announces a security release, even if the pinned minor version has not changed.

## Release checklist

Every release must run the advisory and license gate, review `cargo tree -d`, and confirm that no known reachable vulnerability ships. The step is part of [RELEASING.md](RELEASING.md).

## Residual risk

The crate cannot audit every line of the Turso or Bitcoin Dev Kit dependency trees. Accepted residual risk is limited to advisories that are triaged as unreachable or that have a documented, unexpired exception in `deny.toml`. Encryption keys, database files, and sync credentials remain the deployer's responsibility; see the README for the supported encryption boundaries.
