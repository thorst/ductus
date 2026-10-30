# Introduction

Ductus is an integration engine: it receives messages (HL7 v2 over MLLP, HTTP, files, SFTP,
schedules), runs your code on them, and delivers them reliably to the systems that need them.
Interfaces are written as code (TypeScript, with a typed SDK) and configured in plain YAML files
that live in Git, next to their tests. Messages are stored in PostgreSQL before they're
acknowledged, delivered at least once, in order where you ask for it, and kept for a window you
choose; what happened to each one can be looked up, resent or corrected.

> **Pre-release.** Ductus is under active development and not yet supported in production. These
> docs describe the version they come with; the guides for installing, configuring, operating
> and upgrading are being written now.

## How to read these docs

- **Guides** explain how to do something, start to finish.
- **[Reference](reference/index.md)** pages list everything: every
  [command](reference/cli/index.md), [API operation](reference/api/index.md) and
  [setting](reference/config/index.md), made from the engine itself so they always match it.
- **[Nuances](nuances.md)** say what could surprise you, why it is so, and what to do about it,
  each with a level from 1 (foundations) to 5 (expert).
- **[Architecture](architecture.md)** is how the engine works inside: for the curious, and for
  anyone who must explain its guarantees to someone else.

Every setting, command and page aims to say what it does, why you'd use it, when not to, its
default, and an example. If one doesn't, that's a bug: please
[report it](https://github.com/thorst/ductus/issues).
