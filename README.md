# 42 Tools for Zed

42 Tools is a Rust-based Zed extension for 42 School C workflows. It provides:

- 42 header insert/update actions for `.c` and `.h` files
- formatting through `c_formatter_42`
- bundled macOS and Linux language-server installs for one-click Zed Store setup

## Supported platforms

- macOS `aarch64`
- macOS `x86_64`
- Linux `aarch64`
- Linux `x86_64`

Windows is intentionally unsupported in the first store release.

## Development

Build the extension WASM:

```bash
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/forty_two_tools_extension.wasm extension.wasm
```

Run the LSP test suite:

```bash
cargo test -p forty-two-tools-lsp
```

For local `Install Dev Extension` flows before a GitHub release exists, configure a custom server path in Zed:

```json
{
  "lsp": {
    "42-tools-lsp": {
      "binary": {
        "path": "/absolute/path/to/forty-two-tools-lsp"
      }
    }
  }
}
```

## Zed settings

The extension reads the standard `lsp."42-tools-lsp"` block.

```json
{
  "lsp": {
    "42-tools-lsp": {
      "binary": {
        "path": "/optional/custom/path/to/forty-two-tools-lsp",
        "arguments": []
      },
      "settings": {
        "formatter": {
          "path": "/optional/custom/path/to/c_formatter_42",
          "arguments": []
        },
        "header": {
          "login": "marvin",
          "email_domain": "student.42istanbul.com.tr"
        }
      }
    }
  }
}
```

Resolution order:

- LSP binary: `binary.path` -> bundled versioned asset -> release download
- formatter binary: `settings.formatter.path` -> bundled formatter path from the extension -> `c_formatter_42` on `PATH`
- header identity: `settings.header.login` / `settings.header.email_domain` -> `USER` / `USERNAME` and `student.42istanbul.com.tr`

## Release assets

Each GitHub release tag must match the extension version exactly, for example `v0.1.0`.

Expected asset names:

- `42-tools-mac-aarch64.zip`
- `42-tools-mac-x86_64.zip`
- `42-tools-linux-aarch64.zip`
- `42-tools-linux-x86_64.zip`

Each zip must extract to exactly these files at its root:

- `forty-two-tools-lsp`
- `c_formatter_42`

## GitHub Actions release flow

The release workflow:

- validates tests and the WASM build
- builds `forty-two-tools-lsp` on four target runners
- downloads the matching `c_formatter_42` binary for each target
- packages the expected zip assets
- uploads the assets to the tagged GitHub release

Set these repository variables before tagging a release:

- `FORMATTER_MACOS_AARCH64_URL`
- `FORMATTER_MACOS_X86_64_URL`
- `FORMATTER_LINUX_AARCH64_URL`
- `FORMATTER_LINUX_X86_64_URL`

Create a release by pushing a semver tag:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## Zed Store submission

After publishing the GitHub release:

1. Add this repository as `extensions/42-tools` in `zed-industries/extensions`.
2. Update `extensions.toml` with `version = "0.1.0"`.
3. Run `pnpm sort-extensions`.
4. Open the store PR.
