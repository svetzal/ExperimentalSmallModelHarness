# Install

## Homebrew

Install Microgentic from the `svetzal/tap` tap:

```sh
brew install svetzal/tap/microgentic
```

To upgrade to the newest release:

```sh
brew upgrade svetzal/tap/microgentic
```

The package installs the `microgentic` command and the `adaptive-agent-harness`
compatibility executable. It does not install an `mg` command, because `mg` is
already the name of an Emacs-style editor. To use the short name, add an alias
to your shell:

```sh
alias mg=microgentic
```

## Supported platforms

| Platform | Requirement |
| --- | --- |
| Apple Silicon Mac | macOS 15 or later |
| Intel Mac | macOS 15 or later |
| Linux x86-64 | glibc 2.35 or later |

Windows and Linux ARM packages are not available yet.

## Release downloads

Each [GitHub release](https://github.com/svetzal/microgentic/releases) has an
archive for each platform and a `SHA256SUMS` file. Each archive contains a
`release.json` manifest with the source revision and the binary checksums.

1. Download the archive for your platform and `SHA256SUMS`.
2. Check the archive: `shasum -a 256 -c SHA256SUMS --ignore-missing`.
3. Extract the archive and put `microgentic` on your `PATH`.

## Build from source

You need Rust 1.99 or later, and network access to fetch the locked Mojentic
Git dependency.

```sh
git clone https://github.com/svetzal/microgentic.git
cd microgentic
cargo install --locked --path . --bin microgentic
```

## You also need a model server

Microgentic does not include a model. Before you send your first message, start
an oMLX or Ollama server and install a model on it. See
[Providers and models](providers.md).

## Uninstall

```sh
brew uninstall microgentic
```

Uninstalling leaves your configuration and traces in place, so an upgrade or a
reinstall keeps them. To remove them too, delete the directories listed in
[Files and locations](locations.md).
