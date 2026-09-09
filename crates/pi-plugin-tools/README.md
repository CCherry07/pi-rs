# Native plugin author tools

The native author workflow is available through the pi CLI. It builds on the existing SDK,
loader and installer, with no new plugin lifecycle or ABI. Agent, provider and session plugins
each remain one crate exporting one dynamic library.

## Create and try a plugin

For a released host, the scaffold pins its source commit and Rust toolchain:

~~~sh
pi plugin new hello --kind agent
pi --cwd hello plugin package
pi plugin verify hello/dist
pi plugin install ./hello/dist
~~~

During pi-rs development, use the same working checkout as the host:

~~~sh
cargo run -p pi-cli -- plugin new /tmp/hello --sdk /path/to/pi-rs --kind agent
cargo run -p pi-cli -- --cwd /tmp/hello plugin package --debug
cargo run -p pi-cli -- plugin verify /tmp/hello/dist
~~~

The other kinds are provider and session. Use --name when the crate name should differ from the
destination directory, and --sdk-rev to explicitly pin a full Git commit hash. A local --sdk path
may point at the pi-rs checkout or its crates/pi-plugin-sdk directory.

The scaffold includes a standalone Cargo.toml, source and identity test, rust-toolchain.toml,
seeded Cargo.lock, README, ignore rules, and a GitHub Actions release workflow. The first package
build completes the lock for the new crate; commit it and use --locked in CI. Existing output
directories are never overwritten. Choose a new --output directory for subsequent builds.

SDK consumption still uses a fixed Git revision or a complete local pi-rs checkout. The SDK and
its transitive workspace crates are not published to crates.io. Its current build fingerprint
depends on the workspace source tree and lockfile; changing publish flags alone would not make
an independently published crate usable. The scaffold preserves that exact-build contract.

## Package format and validation

~~~sh
pi plugin package --manifest-path ./Cargo.toml --locked --output dist-next
~~~

Packaging uses Cargo metadata and compiler artifact messages, so custom library names and Cargo
target directories work. It builds the running host's Rust target with the release profile by
default; --debug uses the dev profile. Cross-compilation is intentionally not offered: each
artifact must be loaded and checked on its native runner before publication.

The actual binary descriptor supplies plugin ID, version and kind. The binary version must
match Cargo metadata. The existing loader checks ABI 16, the SDK build fingerprint and the
matching constructor export. Verification can run native library initialization code, but does
not invoke plugin constructors or register hooks.

Declare optional package configuration defaults in Cargo.toml:

~~~toml
[package.metadata.pi-plugin.options]
endpoint = "https://example.com"
~~~

The output directory contains exactly the release files:

~~~text
dist/
  pi-plugin.toml
  pi-plugin-release.json
  hello-0.1.0-aarch64-apple-darwin.dylib
~~~

The dynamic library filename includes version and target to avoid collisions across runners.
The JSON release manifest uses the existing installer schema and relative artifact URLs with
SHA-256 checksums. The TOML manifest allows direct local installation. Only the dynamic library
is packaged: resource directories and source archives are not part of the current native
installer format.

Normal verify checks every listed file, rejects duplicate targets, path escapes, tampered
checksums and disagreement between local/remote manifests, and loads this host's artifact to
check native compatibility. It fails when no artifact matches the host. Foreign-target files
are checksum-checked only. On a machine without a matching artifact, use:

~~~sh
pi plugin verify release --integrity-only
~~~

That mode does not load native code or claim ABI compatibility. A SHA-256 check verifies
integrity, not publisher identity.

## Multiple platforms and publication

The generated workflow pins the SDK commit, builds its matching pi binary on each of the six
product release targets, runs plugin tests with --locked, packages and verifies the native
artifact, then merges those bundles on Linux. A scaffold using --sdk writes a disabled
release.yml.example: replace the Cargo path dependency with a pinned Git dependency, pin that
same revision in the workflow, and rename the workflow before enabling CI.

The workflow runs only on manual workflow_dispatch with an existing plugin version tag. It
uses read-only repository permissions for builds and write permission only for publication.

~~~sh
pi plugin merge bundles/macos-arm64 bundles/linux-x64 --output release
pi plugin verify release
pi plugin publish github --bundle release --repo OWNER/REPO --tag v0.1.0
~~~

Merge requires identical ID, version, kind and options and rejects duplicate targets or filenames.
It checks all files and emits a deterministic manifest; it does not claim to have loaded foreign
binaries. A merged multi-target directory has no local pi-plugin.toml because one local artifact
cannot represent every platform. Install it through the published release URL or GitHub source.

Publishing requires the authenticated GitHub CLI (gh), a matching vX.Y.Z tag already present in
the target repository, and a bundle containing a compatible artifact for the publishing host.
It snapshots the verified files, creates a draft, uploads only those files and the release
manifest, then makes the draft public. Use --draft to keep it private to repository collaborators.
Existing releases/assets are not overwritten. Failure after draft creation leaves that draft for
inspection; recovery/removal is manual. The command never scans the source tree for upload.

Consumers can then use the existing installer:

~~~sh
pi plugin install github:OWNER/REPO@v0.1.0
~~~

To prepare static registry metadata:

~~~sh
pi plugin registry-entry release \
  --manifest-url https://github.com/OWNER/REPO/releases/download/v0.1.0/pi-plugin-release.json
~~~

This prints a validated registry fragment; merge it into the registry's existing index using
the registry's normal review/publication process. It does not overwrite an existing registry.
Signing, OCI, Git source installation, update/rollback and store GC remain separate milestones.
