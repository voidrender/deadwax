# Packaging

These files are templates for the Homebrew tap and the AUR package. Both build from the source tarball of a tagged GitHub release.

## Releasing a version

1. Set `version` in `Cargo.toml`, run `cargo check` so `Cargo.lock` picks it up, and commit.
2. Tag the commit `vX.Y.Z` and push the tag.
3. Get the checksum of the release tarball:

   ```sh
   curl -sL https://github.com/voidrender/senritsu/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
   ```

4. Update the Homebrew formula and the PKGBUILD as described below.

## Homebrew

The formula lives in the `voidrender/homebrew-tap` repository at `Formula/senritsu.rb`. Copy `homebrew/senritsu.rb` there, then set `url` to the new tag and `sha256` to the checksum. Test it with:

```sh
brew install --build-from-source voidrender/tap/senritsu
brew test senritsu
brew audit --strict senritsu
```

On macOS the formula also builds `SenritsuNotifier.app` with `scripts/install-macos-notifier.sh --build-only` and installs it to the formula's `libexec`. Senritsu looks for it there.

## AUR

Copy `aur/PKGBUILD` into a clone of the AUR package repository, set `pkgver`, reset `pkgrel` to 1, and run:

```sh
updpkgsums
makepkg --printsrcinfo > .SRCINFO
makepkg -si
```

Commit `PKGBUILD` and `.SRCINFO` and push to the AUR.
