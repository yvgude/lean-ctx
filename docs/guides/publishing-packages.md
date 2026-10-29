# Research — Context Kit and package material

> **No public hosted registry or marketplace is available.** Do not use this
> document as publication, pricing, publisher-account, entitlement, or
> installation guidance for a LeanCTX service.

## Current status

LeanCTX has a signed local .ctxpkg substrate. First-class Context Kit semantics,
portable package contracts, pinning, signer behavior, and any distribution path
are **Research**. A package implementation artifact does not
establish a public registry, a managed publisher identity, or a commercial
distribution product.

## Safe interpretation

Local engineering work may inspect and test a package only within its declared
repository contract and security boundary. It must not claim that a package is
publicly discoverable, hosted, supported, audited, billable, or installable by
other users.

## Local copies and their limits

- **Held copies stay usable offline.** A package installed or exported is a
  local copy. Yanking or superseding a version in a registry deletes no copy —
  not on this machine and not one that was exported, sent or installed
  elsewhere. LeanCTX makes no remote-deletion claim for packages.
- **Version status is a snapshot.** When `pack install`/`pack update` fetch a
  registry's version list, the engine keeps it beside the local store. Offline,
  a held version is then marked as yanked or superseded *as of that fetch*;
  nothing is fetched or reported to obtain that hint.
- **Auto-load is bound to one project.** `pack auto-load <name>` applies a
  package to the project it was enabled in, and to no other project — a
  same-named project at another root included. Auto-load entries written by
  older versions are bound to no project and load nowhere until enabled again
  in a project.
- **Package facts are data, not instructions.** Facts merged from a package
  reach the model inside an untrusted-data fence and are never written into
  agent rule files; they grant nothing.

## Promotion requirements

A public package path needs a released contract, explicit provenance and
verification semantics, a defined security and revocation model, compatibility
ownership, and a product-status decision. Until then, Context Kits remain a
status-qualified part of the LeanCTX roadmap, not an ecosystem story.
