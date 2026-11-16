# Releasing `bdk_turso`

Releases are published to [crates.io](https://crates.io/crates/bdk_turso) and documented on [docs.rs](https://docs.rs/bdk_turso) by the `Publish` workflow when a `v*` tag is pushed. The workflow verifies the tag, tests, docs, and package contents before it uploads anything.

## One-time setup

### crates.io trusted publishing

Trusted publishing lets the workflow publish with a short-lived OIDC token instead of a long-lived crates.io token. The crate must already exist on crates.io before trusted publishing can be configured.

1. Open the crate settings on crates.io and choose **Trusted Publishing**.
2. Add a GitHub publisher with:
   - repository: `emma31-dev/bdk_turso`
   - workflow: `publish.yml`
   - environment: `crates-io`
3. Optionally enable **Trusted Publishing Only** after the first successful automated release.

The workflow references the `crates-io` GitHub environment. Create it under **Settings → Environments** and add protection rules, such as required reviewers, so a release needs approval before publishing.

### Token fallback

If trusted publishing is unavailable, store a crates.io API token as a repository secret named `CARGO_REGISTRY_TOKEN`. The workflow uses the secret when it is present and falls back to OIDC when it is not. Keep the token scoped to this crate and rotate it after use if it may have been exposed.

```sh
gh secret set CARGO_REGISTRY_TOKEN
```

### docs.rs

`Cargo.toml` sets `[package.metadata.docs.rs]` with `all-features = true`, so docs.rs builds the `wallet`, `psbt`, and `remote` APIs. docs.rs builds automatically after crates.io accepts the release; no separate workflow is required.

## Release checklist

1. **Choose the version.** Follow semantic versioning. For the 0.x series, a patch release must stay backward compatible; put breaking changes in the next minor release.
2. **Bump the version** in `Cargo.toml`. Run `cargo check` so `Cargo.lock` records the new package version.
3. **Update `CHANGELOG.md`.** Move the `[Unreleased]` entries into a new `## [x.y.z] - YYYY-MM-DD` section and link the heading to the crates.io version. Leave an empty `[Unreleased]` section for the next release.
4. **Run the local checks:**

   ```sh
   cargo fmt --all -- --check
   cargo test --all-targets --locked
   cargo test --all-targets --all-features --locked
   cargo check --no-default-features --locked
   RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps --locked
   cargo package --list --locked
   cargo publish --dry-run --locked
   ```

   Confirm that `cargo package --list` includes `README.md`, `migrations/*.sql`, and the examples. The crate reads migrations with `include_str!`, so a missing file breaks downstream builds.
5. **Commit and push** the version bump, changelog, and any release notes. Wait for CI to pass on `main`.
6. **Tag the release** from the commit that passed CI:

   ```sh
   git tag -a v0.3.2 -m "bdk_turso 0.3.2"
   git push origin v0.3.2
   ```

7. **Approve the workflow** if the `crates-io` environment requires reviewers. The `Publish` workflow then:
   - checks that the tag matches `Cargo.toml`;
   - checks that `CHANGELOG.md` has a section for the version;
   - runs the tests, docs, package checks, and a publish dry run;
   - publishes to crates.io;
   - creates a GitHub release from the changelog section.
8. **Verify the release:**
   - crates.io: <https://crates.io/crates/bdk_turso>
   - docs.rs: <https://docs.rs/bdk_turso/x.y.z>
   - GitHub releases: <https://github.com/emma31-dev/bdk_turso/releases>

   docs.rs can take a few minutes. If the build fails, open the build log from docs.rs, fix the problem, and publish a new patch version. docs.rs also provides a rebuild button for the existing version.

## If a release fails

- **Before crates.io accepts the upload:** fix the problem, delete the tag locally and remotely, commit the fix, and create the tag again. Do not move a tag that has already been published.
- **After crates.io accepts the upload:** never reuse the version. Publish a new patch release with the fix. If the published version is unusable, yank it:

  ```sh
  cargo yank --version 0.3.2
  ```

  Yanking prevents new dependency resolution from selecting the version but does not delete it.
- **If the GitHub release fails after publishing:** create it manually with `gh release create v0.3.2 --title "bdk_turso 0.3.2" --notes-file release-notes.md`, using the changelog section for the notes.
- **If docs.rs fails:** fix the docs build in a new patch release, or trigger a rebuild from the docs.rs build page after correcting the source.

## Release cadence

The project follows the milestone and weekly progress cadence described in the grant roadmap. Patch releases may ship earlier for security or correctness fixes. Do not hold a small fix for a larger feature release.
