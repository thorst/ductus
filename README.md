# Ductus

**A code-first integration engine for healthcare.** Write interfaces in TypeScript, run them on a fast Rust runtime, and operate them with monitoring, replay and data lifecycle tools built in.

> Early development. Nothing here is usable yet.

## Goals

- **Code-first:** interfaces are TypeScript plus a small YAML config, stored in Git, and promoted dev → test → prod.
- **Fast and reliable:** Rust engine with embedded V8. Messages are persisted before they're acknowledged, so a crash loses nothing.
- **HL7 that reads well:** `msg.PID[3][1]`, with lazy parsing and hover docs for every field.
- **Built for operations:** heartbeats, behind-by alerts, time-until-recovery, replay, quarantine, deduplication, and ordering by business key (e.g. MRN).
- **One binary + PostgreSQL:** nothing else to install.

## License

Apache-2.0. See [LICENSING.md](LICENSING.md) for what's open source, what's commercial, and our commitments.
