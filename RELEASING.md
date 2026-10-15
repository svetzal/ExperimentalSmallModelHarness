# Releasing Microgentic

Microgentic publishes versioned binaries through GitHub Releases and `svetzal/tap`.
The initial version is `0.1.0`. It is an early release under active development.

## Supported packages

| Archive | Platform |
| --- | --- |
| `microgentic-darwin-arm64.tar.gz` | Apple Silicon, macOS 15 or later |
| `microgentic-darwin-x64.tar.gz` | Intel Mac, macOS 15 or later |
| `microgentic-linux-x64.tar.gz` | Linux x86-64 with glibc 2.35 or later |

Each archive includes `microgentic`, the compatibility executable, the license,
and `release.json`. The manifest records the source revision and binary checksums.
`SHA256SUMS` records the archive checksums. Windows and Linux ARM packages remain
outside the current release matrix. Source builds need Rust 1.99 or later and
network access for the locked Mojentic Git dependency.

## Cut a release

Publishing needs user authorization. A direct request to publish supplies it.

1. Update the version in `Cargo.toml`.
2. Update `Cargo.lock` with Cargo.
3. Add release notes in `docs/releases/vVERSION.md`.
4. Run the checks in `AGENTS.md`.
5. Commit the release preparation.
6. Push the commit to `main`, and create and push an annotated `vVERSION` tag
   at that commit. The tag starts the release workflow. Do not wait for the
   `main` CI run first: the release workflow runs the same platform checks and
   publishes nothing unless all of them pass.
7. Install the release locally with Cargo (see `AGENTS.md`) and continue work.
   Let both workflow runs finish; never cancel them.
8. Later, when the release workflow has passed, download the archives and
   `SHA256SUMS`, and check the archive checksums.
9. Update `Formula/microgentic.rb` in `svetzal/homebrew-tap` with the version and
   checksums.
10. Test the formula before publishing the tap commit.

If the release workflow fails, nothing is published. Fix the cause, then cut a
new patch version rather than replacing the tag.

The tag must match the Cargo version. The workflow checks formatting, lint,
tests, collection validation, and installed binaries on native runners.
It publishes assets only after all platform jobs pass. Each release build embeds
`MICROGENTIC_RELEASE_REVISION`; development builds retain live-checkout provenance.
Release packaging refuses a dirty source checkout or mismatched revision.

The tap update is a separate, reviewed commit. The release workflow needs no
credential that can write another repository. Future releases use the same steps.
Never replace an existing release tag or asset silently.

## Check the installation

```sh
brew install svetzal/tap/microgentic
brew test svetzal/tap/microgentic
microgentic --version
microgentic setup --apply
microgentic
```

Provider setup needs a running oMLX or Ollama server when you send a request.
Setup itself makes no provider request. `mg` remains an optional user-defined alias.
The compatibility executable preserves the old launch form for benchmark consumers.

Uninstall with `brew uninstall microgentic`. Saved configuration and traces remain
in their documented XDG locations so an upgrade or reinstall can retain them.
