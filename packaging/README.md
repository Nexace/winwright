# Packaging

## winget

Every release attaches `winget-manifests.zip`: the three files in [winget](winget) with the
version and the zip's SHA-256 filled in. Once the repository is public, submit them to
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) under
`manifests/n/Nexace/Winwright/<version>/` (by pull request, or `wingetcreate submit <folder>`).
Check them first with `winget validate <folder>`.

## Code signing

Unsigned downloads make Windows SmartScreen warn "unknown publisher". Signing needs a code-signing
certificate in the maintainer's name (an OV certificate, or Azure Trusted Signing). With one:

1. Store it as repository secrets (for Trusted Signing: the account, profile and an Entra app).
2. Add a signing step to `.github/workflows/release.yml` right after the release build, signing
   `target/release/winwright.exe` (`signtool sign /fd SHA256 /tr <timestamp url> /td SHA256 ...`,
   or the `azure/trusted-signing-action`).

Reputation builds over time even when signed; a signed build starts out with fewer warnings.
