# Licensing & Our Commitments

We want to be clear about how this project is licensed and how we intend to make money, so nobody is surprised later.

## The short version

- **The core engine is open source under Apache-2.0, and it stays that way.**
- A small set of **Enterprise features** for large organizations is sold under a commercial license.
- The free edition is a complete, production-ready integration engine, not a trial or a demo.

## What's open source (Apache-2.0)

Everything you need to build, run and operate interfaces in production on a single server:

- The engine: connectors, HL7 and other formats, routing, ordering, retries, ACK/NAK, quarantine, deduplication, rate limiting
- Message storage, retention, search and replay
- The TypeScript SDK, CLI, test framework and validation
- The web admin, monitoring and alerting
- The VS Code extension
- User management with LDAP and standard roles
- Documentation and examples

You can use it commercially, modify it and redistribute it under the terms of Apache-2.0. You don't have to talk to us or pay us.

## What's commercial (Enterprise)

Features that mainly matter to large, multi-site organizations:

- Multi-node clustering, automatic failover and zero-downtime rolling upgrades
- Archive tiers (object storage), search across archives, compliance (WORM) storage
- SSO (SAML/OIDC) and fine-grained permissions
- Advanced analytics, forecasting and the architecture advisor
- Support with SLAs

## Our commitments

1. **We won't relicense the core.** Code released under Apache-2.0 stays Apache-2.0. We will not move the open-source engine to a closed or source-available license.
2. **We won't move features from free to paid.** Once a feature ships in the open-source edition, it stays there.
3. **No crippleware.** We won't hold back reliability, data safety or security fixes from the free edition to push upgrades.
4. **Your data and interfaces are yours.** Interfaces are plain files in Git. Message data lives in your own PostgreSQL. Nothing locks you in.
5. **Contributions stay open.** Contributions go into the open-source core under Apache-2.0. We use a DCO sign-off, not a CLA, because we don't need extra rights over your work.

## Why we chose this model

Integration engines run critical healthcare workflows for many years. Organizations need confidence that the tool they standardize on won't change terms underneath them. An OSI-approved license gives you that confidence, and it passes legal and procurement review without friction.

Enterprise licenses fund full-time development, so the open-source engine keeps getting better.

## Questions

If something here is unclear, or you think a commercial feature belongs in the free edition, open a discussion. We'd rather hear it than guess.
