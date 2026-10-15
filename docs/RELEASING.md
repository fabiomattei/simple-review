# Releasing Simple Review

Installers are built with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager)
from the configuration in `crates/app/Cargo.toml` (`[package.metadata.packager]`).

| Platform | Formats | Update bundle |
| --- | --- | --- |
| macOS (Apple Silicon, Intel) | `.app`, `.dmg` | `.app.tar.gz` |
| Windows | `.msi` (WiX), `-setup.exe` (NSIS) | the NSIS installer |
| Linux | `.AppImage`, `.deb` | the AppImage |

The app needs the `git` command at runtime (blame and history); the `.deb` depends on it, and
the macOS/Windows installers expect it to be installed already.

## Building locally

```sh
cargo install cargo-packager --locked --version 0.11.8
cd crates/app
CI=true cargo packager --release --formats app,dmg     # macOS
cargo packager --release --formats wix,nsis            # Windows
cargo packager --release --formats appimage,deb        # Linux
```

Packages land in `target/packages/`. `CI=true` skips the AppleScript that arranges the DMG
window, which needs permission to control Finder.

## Releasing with GitHub Actions

1. Bump `version` in the workspace `Cargo.toml`, commit.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.
3. `.github/workflows/release.yml` builds every platform, signs what it has credentials for,
   writes `latest.json` (the update manifest) and creates a **draft** GitHub release.
4. Check the draft, write release notes, publish.

Without any secrets the workflow still produces unsigned installers and no update bundles.

## Code signing (credentials to be provided)

### macOS: Developer ID + notarization

1. In the Apple Developer account create a **Developer ID Application** certificate and export
   it from Keychain Access as a `.p12` with a password.
2. Create an app-specific password for the Apple ID used for notarization.
3. Add repository **secrets**:
   - `APPLE_CERTIFICATE`: `base64 -i certificate.p12`
   - `APPLE_CERTIFICATE_PASSWORD`: the `.p12` password
   - `APPLE_SIGNING_IDENTITY`: e.g. `Developer ID Application: Fabio Mattei (TEAMID1234)`
   - `APPLE_ID`, `APPLE_PASSWORD` (the app-specific password), `APPLE_TEAM_ID`

cargo-packager imports the certificate into a temporary keychain, signs the `.app` with the
hardened runtime, and notarizes it when the Apple ID variables are set.

### Windows: Authenticode

1. Obtain a code-signing certificate (OV or EV) as a `.pfx`. EV certificates on hardware tokens
   or cloud HSMs need a `sign-command` instead; see cargo-packager's `windows.sign-command`.
2. Add repository **secrets**:
   - `WINDOWS_CERTIFICATE`: `base64` of the `.pfx`
   - `WINDOWS_CERTIFICATE_PASSWORD`
   The workflow imports it and sets `certificate-thumbprint`; SHA-256 digests are timestamped
   with DigiCert's server (see `[package.metadata.packager.windows]`).

### Linux

AppImage and `.deb` are not signed. Publish SHA-256 sums with the release if you want users to
verify downloads.

## Auto-update

Updates are **off by default**: a build only contacts an update server if it was compiled with an
endpoint and public key, the `updater` feature (release builds enable it), and the user turned on
"Check for updates at startup" or clicked "Check now" in Preferences. Updates are verified against
the public key before installing.

One-time setup:

1. Generate the signing key pair (keep the private key secret and backed up; losing it means
   existing installs can no longer verify updates):
   ```sh
   cargo packager signer generate --path ~/.simple-review/updater.key
   ```
2. Repository **secrets**: `UPDATER_PRIVATE_KEY` (contents of `updater.key`) and
   `UPDATER_PRIVATE_KEY_PASSWORD`.
3. Repository **variables**:
   - `UPDATE_PUBKEY`: contents of `updater.key.pub`
   - `UPDATE_ENDPOINT`: where `latest.json` is served, e.g.
     `https://github.com/<owner>/<repo>/releases/latest/download/latest.json`

The release workflow uploads `latest.json` with the installers; once the release is published,
that URL serves the newest manifest. `scripts/collect_artifacts.py` and
`scripts/update_manifest.py` produce it and can be run locally to inspect the output.
