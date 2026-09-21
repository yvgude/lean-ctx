# Windows code signing

The release workflow signs both Windows engines (MSVC and GNU/jemalloc) with
Azure Artifact Signing before creating their ZIP archives and Python companion
wheels. The publisher is **Thinkery AG**. Historical release files are not
retroactively signed by this change.

## Configuration

- Azure tenant: Thinkery AG.
- Account: `thinkery-signing`, region `Switzerland North`.
- Profile: `leanctx-public-trust`, Public Trust.
- Endpoint: `https://swn.codesigning.azure.net/`.
- Entra application: `leanctx-github-signing`.
- GitHub environment: `windows-signing`, allowed branch `main` and tags `v*`.
- Environment variables: `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`,
  `AZURE_SUBSCRIPTION_ID` (identifiers, not secrets).

The application has `Artifact Signing Certificate Profile Signer` only at the
`thinkery-signing/certificateProfiles/leanctx-public-trust` resource scope.
It authenticates without client secrets through GitHub OIDC:

```text
issuer:   https://token.actions.githubusercontent.com
subject:  repo:yvgude/lean-ctx:environment:windows-signing
audience: api://AzureADTokenExchange
```

This repository currently uses the default, non-immutable OIDC subject. Before
changing its name, owner or OIDC subject format, update the Azure trust to match.
Keep environment branch/tag restrictions in place. The signing action also
rejects pull requests and only accepts release-tag pushes or manual main builds.

## Verification and recovery

Run **Windows signing verification** from the Actions tab on `main`, or:

```sh
gh workflow run windows-signing-check.yml --repo yvgude/lean-ctx --ref main
```

This builds both production engines, signs them using the same action as the
release workflow, runs the signed binaries, and checks that ZIP and wheel
contents exactly match the verified signed EXE. It uploads verification
artifacts for 14 days without publishing a release or packages.

Signature verification fails closed unless Authenticode is valid, the publisher
is Thinkery AG, and a timestamp certificate is present. Signing explicitly uses
SHA256 and RFC3161 timestamping at `http://timestamp.acs.microsoft.com` with
SHA256. Azure leaf certificates are short-lived: never remove timestamping.

- OIDC login failure: compare the actual issuer, subject and audience against
  the application's federated credential and check the environment variables.
- Signing 403: check the profile-scoped signer role and the Switzerland North
  endpoint. A recent role assignment may need time to propagate.
- Invalid signature or missing timestamp: stop publication and investigate the
  signing logs; do not add a skip-signing or continue-on-error fallback.
- Expired organization validation: renew it in Azure before creating new
  signatures; do not replace approved organization details casually.

A valid Authenticode signature is evidence of publisher identity and integrity.
The hosted Windows runner test does not establish Smart App Control acceptance
on a clean Windows 11 installation; that remains a separate release check.

References: [Azure roles](https://learn.microsoft.com/azure/artifact-signing/tutorial-assign-roles),
[signing action](https://github.com/Azure/artifact-signing-action),
[GitHub OIDC](https://docs.github.com/actions/reference/security/oidc).
