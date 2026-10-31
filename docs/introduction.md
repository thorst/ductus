# Introduction

Ductus is an integration engine: it receives messages, runs your code on them, and delivers them
reliably to the systems that need them. Messages come in over HL7 v2's MLLP, HTTP, files in a
folder or on an SFTP server, or are made on a schedule; they go out the same ways, or into
another interface's queue. Interfaces are written as code (TypeScript, with a typed SDK) and
configured in plain YAML files that live in Git, next to their tests.

Every message is stored in PostgreSQL before the sender is told it arrived, delivered at least
once, in order where you ask for it, and kept for a window you choose. What happened to each one
can be looked up, sent again or corrected, by people with the right to, and every such action is
in the audit log.

> **Pre-release.** Ductus is under active development. Downloads before v1.0 are development
> builds: there is no production support, there has been no external security audit, and the
> Windows installer isn't signed yet. Try it, test it, tell us what's missing; don't send real
> patient data through it yet.

## What it is, and what it isn't

**It is** one program (`ductus`, with its web UI inside) and a PostgreSQL database. There is no
message broker, no Java runtime and no Node.js to install: the engine runs your TypeScript in an
embedded V8, and uses Postgres as its store and its queue. It runs as a service on Linux
(systemd) or Windows, in a container, or on a laptop with a database of its own
([`ductus dev`](install/dev.md)).

**It is for** the work an interface team does: feeds from an EHR to a lab or billing system, an
API that other systems call and wait on, a nightly file from a partner, a lookup in a database
on the way. It's written with healthcare in mind (HL7 v2 is a first-class format, retention and
the audit log follow HIPAA's habits), and it works the same for any feed, API or batch job.

**It isn't** a low-code tool: there are no drag-and-drop mappings. Simple interfaces are a few
lines of YAML (a feed relayed as it is needs no code at all), and anything more is TypeScript
with types, tests and an editor that knows HL7's field names. It isn't a database or an archive
either: messages are kept for one, three or seven weeks, and longer history belongs in the
systems that produced it. And before v1.0 it doesn't cluster: one engine runs each interface,
and a standby is started by a person ([why](nuances.md#a-dead-engines-interfaces-are-free-at-once-a-vanished-hosts-within-25-s-level-3)).

## Where to start

| You want to | Read |
|---|---|
| Decide where to run it | [Choosing Linux or Windows](choosing-linux-or-windows.md) |
| Try it on your own machine in ten minutes | [On your laptop: `ductus dev`](install/dev.md), then [Your first interface](first-interface.md) |
| Install it on a server | [Install](install/index.md), [PostgreSQL](install/postgres.md), [Ports and firewalls](ports.md) |
| Set up a project for real | [The project](configure/index.md) and the pages after it |
| Know what could surprise you | [Nuances](nuances.md), by topic and by level |

## How to read these docs

- **Guides** (Install, Configure and the pages around them) explain how to do something, start
  to finish, and why each step is there. They link to the reference for every setting they name.
- **The [reference](reference/index.md)** lists everything: every
  [command](reference/cli/index.md), [API operation](reference/api/index.md) and
  [setting](reference/config/index.md), with its default and an example. It's made from the
  engine itself, so it always matches the version it comes with.
- **[Nuances](nuances.md)** say what could surprise you, why it is so, and what to do about it,
  each with a level from 1 (everyone needs it on day one) to 5 (expert). Guides link to them
  rather than repeat them.
- **[Architecture](architecture.md)** is how the engine works inside: for the curious, and for
  anyone who must explain its guarantees to someone else.

Every setting, command and page aims to say what it does, why you'd use it, when not to, its
default, and an example. If one doesn't, that's a bug: please
[report it](https://github.com/thorst/ductus/issues).
