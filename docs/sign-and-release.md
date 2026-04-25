# Signing and releasing a Power Remote Desktop MSI

Phase 4 G5 ships the signing scaffolding (script + docs) but does NOT include a code-signing certificate. This guide is the runbook for the day a cert is procured.

## Choosing a certificate

Public OSS distribution on Windows hits **SmartScreen** — Windows checks the publisher's signature on every installer run. Three options:

| Option | Cost | SmartScreen | Notes |
|---|---|---|---|
| **EV (Extended Validation)** | $300+/year | Trusted immediately | Requires hardware token (USB key); validation 1-2 weeks |
| **OV (Organization Validation)** | $100+/year | Warns until reputation builds (~weeks of installs) | File-based cert, easy to use in CI |
| **Self-signed** | $0 | Always warns | Test only, not for public release |

Common vendors: Sectigo, DigiCert, SSL.com. EV cert procurement involves identity / business verification — start the process several weeks before you intend to ship.

## Storing the cert

- **Local dev**: keep the `.pfx` outside the repo (e.g. `~/secrets/prdt-codesign.pfx`). Never commit.
- **CI**: store as an encrypted secret (GitHub Actions: `secrets.CODESIGN_PFX_BASE64`, decode at job start to a temp file). The MSI workflow runs `scripts/sign-msi.ps1` after `cargo wix`.

## Using `scripts/sign-msi.ps1`

```powershell
scripts/sign-msi.ps1 `
    -CertPath "C:\path\to\prdt-codesign.pfx" `
    -CertPassword "<password>" `
    -MsiPath "target/wix/prdt-setup-v0.0.1.msi"
```

What it does:

1. Validates the cert file and MSI exist.
2. Runs `signtool sign /f <cert> /p <pass> /t <timestamp_url> /td sha256 /fd sha256 /d "Power Remote Desktop" /v <msi>`.
3. Runs `signtool verify /pa /v <msi>` to confirm the signature is valid and the timestamp is trusted.

Pass a different `-TimestampUrl` if `timestamp.digicert.com` is unreachable. Backup options:

- `http://timestamp.sectigo.com`
- `http://tsa.starfieldtech.com`
- `http://timestamp.globalsign.com`

## Release checklist

After a green build:

1. `version` bumped in workspace `Cargo.toml`.
2. `cargo run -p prdt-gui-host --bin mkicon`.
3. `cargo build --release -p prdt-host -p prdt-viewer -p prdt-viewer-overlay`.
4. `cargo wix --no-build`.
5. **Sign**: `scripts/sign-msi.ps1 -CertPath ... -MsiPath target/wix/prdt-setup-vX.Y.Z.msi`.
6. `git tag -a vX.Y.Z` matching workspace version.
7. `git push && git push --tags`.
8. `gh release create vX.Y.Z target/wix/prdt-setup-vX.Y.Z.msi --notes-file CHANGELOG-vX.Y.Z.md`.
9. Verify the auto-update path (G4) by running an installed older `prdt-host.exe` and checking that Settings → Check for updates surfaces the new version.

## Troubleshooting

- **`signtool: unknown error 0x80092009`** — Cert format mismatch. Ensure the `.pfx` was exported with the private key included.
- **`The specified timestamp server either could not be reached`** — Try a different `-TimestampUrl`.
- **SmartScreen still warns after signing** — That's expected with OV certs until enough installs accumulate "reputation". Microsoft's algorithm; nothing to do but wait or upgrade to EV.
- **`signtool verify` fails after sign succeeds** — A timestamp service mismatch or trust root issue. Run `certutil -store My` to inspect the local cert store.
