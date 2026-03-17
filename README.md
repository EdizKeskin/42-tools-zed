# 42 Tools for Zed

42 Tools is a Rust-based Zed extension for 42 School C workflows. It provides:

- 42 header insert/update actions for `.c` and `.h` files
- formatting through `c_formatter_42`
- bundled macOS and Linux language-server installs for Zed Store setup

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
- formatter binary: `settings.formatter.path` -> `c_formatter_42` on `PATH`
- header identity: `settings.header.login` / `settings.header.email_domain` -> `USER` / `USERNAME` and `student.42istanbul.com.tr`

## Formatter installation

`c_formatter_42` is not bundled with the extension release. Users must install it separately and either:

- make `c_formatter_42` available on `PATH`, or
- set `lsp."42-tools-lsp".settings.formatter.path`

If the formatter is missing, header actions still work and the server logs a warning when formatting is requested.

## Release assets

Each GitHub release tag must match the extension version exactly, for example `v0.1.2`.

Expected asset names:

- `42-tools-mac-aarch64.zip`
- `42-tools-mac-x86_64.zip`
- `42-tools-linux-aarch64.zip`
- `42-tools-linux-x86_64.zip`

Each zip must extract to exactly this file at its root:

- `forty-two-tools-lsp`

## GitHub Actions release flow

The release workflow:

- validates tests and the WASM build
- builds `forty-two-tools-lsp` on four target runners
- packages the expected zip assets
- uploads the assets to the tagged GitHub release

Create a release by pushing a semver tag:

```bash
git tag v0.1.2
git push origin v0.1.2
```

## Zed Store submission

After publishing the GitHub release:

1. Add this repository as `extensions/42-tools` in `zed-industries/extensions`.
2. Update `extensions.toml` with `version = "0.1.2"`.
3. Run `pnpm sort-extensions`.
4. Open the store PR.
