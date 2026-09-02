# lean-ctx — GA Documentation Pack

> **Status: historical operations pack — not a GA, Cloud, team, or enterprise
> availability claim.** Use current local Runtime documentation and `lean-ctx
> doctor` to establish behavior. LeanCTX is **The Context SDK for AI Agents**;
> canonical product scope and status are in
> `docs/internal/README.md` (internal, not in this repository).

## Overview

This pack is for administrators deploying lean-ctx, operators running it, and
developers integrating it with AI coding tools. Start with the install guide,
then use the role-specific guide that matches your responsibility.

## Guides

| Guide | Audience | Description |
|---|---|---|
| [Install Guide](install-guide.md) | Admin | Fresh installation on macOS, Linux, and Docker gateway deployments |
| [Upgrade Guide](upgrade-guide.md) | Admin | Version upgrades, rollback, and migration checks |
| [Admin Guide](admin-guide.md) | Admin | Configuration, users, and policies |
| [Monitoring Guide](monitoring-guide.md) | Operator | Metrics, alerts, and dashboards |
| [Developer Guide](developer-guide.md) | Developer | Integration, extension, and customization |
| [API Guide](api-guide.md) | Developer | REST and MCP API reference summary |
| [DR & Backup Guide](dr-backup-guide.md) | Operator | Disaster recovery and backup/restore |
| [Runbook Index](runbook-index.md) | Operator | Incident response procedures |
| [Release Checklist](release-checklist.md) | Admin | Pre- and post-release verification |

## Recommended path

1. Follow the [Install Guide](install-guide.md) for a new machine or service.
2. Run `lean-ctx doctor` and `lean-ctx status` before admitting users.
3. Read the [Admin Guide](admin-guide.md) before changing organization-wide
   configuration or policies.
4. Use the [Upgrade Guide](upgrade-guide.md) for every version change.

## Quick links

- [Full reference documentation](../reference/)
- [Setup and onboarding journey](../reference/01-setup-and-onboarding.md)
- [Lifecycle and troubleshooting reference](../reference/06-lifecycle.md)
- [Contract specifications](../contracts/)
- [Changelog](../../CHANGELOG.md)

## Support boundary

The guides use only public commands and repository artifacts. Capture
`lean-ctx doctor --json` and `lean-ctx status --json` before escalating an
installation or operational issue.
