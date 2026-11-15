# Packaging

These files are templates for the Homebrew tap and the AUR package. Both build from the source tarball of a tagged GitHub release. The release workflow (`.github/workflows/release.yml`) fills in the version and checksum and publishes both.

## Releasing a version

1. Set `version` in `Cargo.toml`, run `cargo check` so `Cargo.lock` picks it up, and commit to `main`.
2. Tag the commit and push the tag:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

The tag starts the release workflow, which:

1. Stops if the tag doesn't match the version in `Cargo.toml`.
2. Runs CI on the tagged commit.
3. Creates the GitHub release with generated notes.
4. Installs, tests and audits the formula on macOS and Linux from a local tap, then commits it to your tap.
5. Builds the AUR package in an Arch Linux container, running its tests, then pushes `PKGBUILD` and `.SRCINFO` to the AUR.

Rerunning a failed job is safe. An existing release is kept, and a formula or PKGBUILD that hasn't changed isn't committed again.

## Dry run

The release dry run (`.github/workflows/release-dry-run.yml`) does what a release does to the formula and the AUR package, but publishes nothing. It builds a tarball of the current commit the way GitHub does, then installs, tests and audits the formula on macOS and Linux and builds the AUR package with its `check()` on Arch Linux. It runs on pull requests that change the packaging, the release scripts, the notifier, or `Cargo.toml`, and you can start it on any branch from the Actions tab.

The dry run never touches the tap or the AUR, so it doesn't check the two secrets. To check them before tagging:

```sh
# The tap token: lists the tap's branches, or fails if the token can't read it.
git ls-remote "https://x-access-token:$TOKEN@github.com/voidrender/homebrew-tap.git"

# The AUR key: prints the AUR's help text when the key is accepted.
ssh -i ~/.ssh/aur-deadwax aur@aur.archlinux.org help
```

## One-time setup

The workflow needs two secrets in the `deadwax` repository. Without one of them, its job logs a warning and skips publishing.

### Homebrew tap

1. Create a public repository named `homebrew-tap` under the same owner as `deadwax`. It can start empty.
2. Create a fine-grained personal access token with access to that repository only, and the "Contents: Read and write" permission.
3. Save it as the `HOMEBREW_TAP_TOKEN` secret:

   ```sh
   gh secret set HOMEBREW_TAP_TOKEN
   ```

### AUR

1. Sign in to the AUR, or create an account.
2. Create an SSH key just for releases, without a passphrase, since the workflow can't type one:

   ```sh
   ssh-keygen -t ed25519 -N "" -C "deadwax releases" -f ~/.ssh/aur-deadwax
   ```

3. Add `~/.ssh/aur-deadwax.pub` under My Account → SSH Public Key on the AUR.
4. Save the private key as the `AUR_SSH_PRIVATE_KEY` secret:

   ```sh
   gh secret set AUR_SSH_PRIVATE_KEY < ~/.ssh/aur-deadwax
   ```

The first push creates the `deadwax` package on the AUR.

## By hand

The workflow's render steps are scripts you can run yourself:

```sh
sha=$(curl -sL https://github.com/voidrender/deadwax/archive/refs/tags/v0.2.0.tar.gz | shasum -a 256 | cut -d' ' -f1)
scripts/release/render-formula.sh 0.2.0 "$sha" > Formula/deadwax.rb   # in a clone of the tap
scripts/release/render-pkgbuild.sh 0.2.0 "$sha" > PKGBUILD            # in a clone of the AUR repo
makepkg --printsrcinfo > .SRCINFO
```

Test the formula with:

```sh
brew install --build-from-source voidrender/tap/deadwax
brew test deadwax
brew audit --strict deadwax
```

On macOS the formula also builds `DeadwaxNotifier.app` with `scripts/install-macos-notifier.sh --build-only` and installs it to the formula's `libexec`. Deadwax looks for it there.
