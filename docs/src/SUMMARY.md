# Summary

[Main Page](index.md)

# User Guide

- [Getting started](guide/getting-started.md)
- [Concepts](guide/concepts.md)
- [Vault, check-out and check-in](guide/vault.md)
- [Files, versions and 3D preview](guide/files.md)
- [BOM, references and change propagation](guide/structure.md)
- [Finding similar parts](guide/similar-parts.md)
- [Change requests, change orders and notifications](guide/changes.md)
- [Simulations (FEA, CFD)](guide/simulation.md)
- [As-built records](guide/as-built.md)
- [Desktop app](guide/desktop.md)
- [Ask TeamCraft (assistant)](guide/assistant.md)
- [Manual test tour](guide/tour.md)

# Administration

- [Install with Docker](admin/docker.md)
- [Configuration](admin/configuration.md)
- [Users, roles and single sign-on](admin/users.md)
- [Read permissions and restricted groups](admin/permissions.md)
- [Event webhooks](admin/webhooks.md)
- [Branding: your own logos](admin/branding.md)
- [Upgrading and rolling back](admin/upgrade.md)
- [Operations](admin/operations.md)
- [Support policy](admin/support-policy.md)

# Reference

- [Supported file formats](reference/formats.md)
- [REST API](reference/api.md)
- [MCP server (AI assistants)](reference/mcp.md)
- [Architecture](reference/architecture.md)
- [FieldCraft and SolidCraft](reference/interop.md)
- [Evaluation of other PLM systems](reference/evaluation.md)
- [Rust API (rustdoc)](reference/rustdoc.md)

# Development

- [Test coverage map](dev/coverage.md)
- [Fuzzing the file readers](dev/fuzzing.md)
- [Threat model](dev/threat-model.md)
- [Accessibility](dev/accessibility.md)
- [Release checklist](dev/release-checklist.md)
- [Architecture decision records](dev/adr/index.md)
    - [0001 Read access model](dev/adr/0001-access-model.md)
    - [0002 Outbox for notifications and webhooks](dev/adr/0002-outbox.md)
    - [0003 Content-addressed vault](dev/adr/0003-content-addressed-vault.md)
    - [0004 No Node.js, no bundler](dev/adr/0004-no-node.md)

# Research

- [Research notes](research/index.md)
    - [3D preview of NX parts](research/nx-jt-preview.md)
    - [Text from PDF reports](research/pdf-text-extraction.md)
    - [One provider interface for the assistant](research/agent-interface.md)
    - [Sharing UI code between web and desktop](research/shared-ui.md)
    - [Supersede, replace and alternate parts](research/supersede-alternates.md)
    - [Jira connector for change requests and change orders](research/jira-connector.md)
    - [Previews of native CAD files](research/cad-previews.md)
    - [Importing existing CAD folder trees](research/cad-folder-import.md)
    - [Popular features of Teamcenter](research/teamcenter.md)
    - [Openly licensed CAD files for end-to-end tests](research/cad-test-files.md)
    - [Read permissions for project groups](research/read-permissions.md)
    - [As-built records](research/as-built.md)
    - [Fluent, STAR-CCM+ and Simcenter 3D files](research/solver-files.md)
