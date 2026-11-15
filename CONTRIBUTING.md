# Contributing to Deadwax

## Building and testing

`rust-toolchain.toml` pins the Rust version that CI uses, and rustup installs it on the first build. The crate's minimum supported version is 1.90.

```sh
cargo run
cargo test
```

Before you open a pull request, run the same checks as CI:

```sh
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --all-targets --locked
cargo test --locked
```

The tests in `tests/mpv_backend.rs` run a real mpv and are skipped when mpv is not installed. Set `DEADWAX_REQUIRE_MPV=1` to make a missing mpv fail them instead, as CI does.

## macOS signing for development

On macOS, keychain access belongs to the exact build that was granted it. Cargo gives every build a new ad-hoc signature, so without a fix macOS asks for your login password after each rebuild. To stop that, create a self-signed signing identity once:

```sh
scripts/make-dev-cert.sh
```

After that, `cargo run` signs the binary with the "Deadwax Dev" identity before starting it (see `.cargo/config.toml` and `scripts/macos-dev-runner.sh`). The script asks for your login password in the terminal, so `codesign` can use the new key without a dialog. If your keychain already holds a Deadwax password saved by a differently signed build, the first launch asks for keychain access once. Choose Always Allow. To sign with a different identity, set `DEADWAX_SIGN_IDENTITY` to its name.

## The macOS notification helper

macOS only shows notifications from a signed app bundle, so Deadwax sends them through `DeadwaxNotifier.app`, built from `macos/`. `scripts/install-macos-notifier.sh` builds and installs it. Pass `--build-only` to leave it in `target/notifier/DeadwaxNotifier.app` instead.

Deadwax looks for the helper in this order:

1. The path in `DEADWAX_NOTIFIER_APP`, when it is set.
2. `../libexec/DeadwaxNotifier.app` relative to the `deadwax` executable, after resolving symlinks. The Homebrew formula installs it there.
3. `~/Applications/DeadwaxNotifier.app`.

## Releases

See [packaging/README.md](packaging/README.md) for how tags publish to Homebrew and the AUR.

## License

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in Deadwax, as defined in the Apache-2.0 license, is dual licensed under the MIT and Apache 2.0 licenses, without any additional terms or conditions.
