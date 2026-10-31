# Documentation fields

Every company keeps things about its interfaces that Ductus doesn't ask for: the business
associate agreement, the change ticket that approved it, the analyst, a runbook, the go-live
date. **Documentation fields** keep them beside the interface, in Git, checked like the rest,
instead of in a spreadsheet nobody updates.

**What you get:** `ductus validate` reports a missing or wrong value; the web UI shows the fields
on each interface, destination and resource's page and filters the interface list by them;
`ductus docs` lists them (`--missing` for what's not filled in, `--json` for a report). **What you
don't:** the engine ignores them entirely; a wrong value never stops anything running.

## Declare them: `docs-schema.yaml`

At the top of the project, [`fields`](../reference/config/docs-schema.md#fields), by the key
values will use:

```yaml
# docs-schema.yaml
fields:
  baa_signed:
    type: enum
    values: ["yes", "no", n/a]
    required: true
    label: BAA signed
    help: Business associate agreement, from the contracts office
  change_ticket:
    type: text
    label: Change ticket
  analyst:
    type: person
    applies_to: [interface, destination]
  runbook:
    type: url
  go_live:
    type: date
  contract:
    type: text
    applies_to: [resource]
```

| `type` | A value is | Example |
|---|---|---|
| `text` | Any text (quote numbers and yes/no, or YAML reads them otherwise) | `"CHG-1234"` |
| `enum` | One of `values`, exactly | `"yes"` |
| `date` | A date | `2026-10-30` |
| `url` | An `http(s)://` address, shown as a link | `https://wiki.hospital.org/adt` |
| `person` | Who, best as an email address | `ana@hospital.org` |
| `list` | Several texts (each one of `values`, if given) | `[lab, billing]` |

- **`required: true`**: every interface (or destination, or resource) the field applies to must
  have a value; `ductus validate` reports each missing one. The engine starts anyway.
- **`applies_to`**: `interface` (the default), `destination`, `resource` (resources.yaml
  entries), or several.
- **`label`** and **`help`**: what the web UI shows instead of the key, and a hint under it.

## Fill them in: `custom:`

Values go in a flat `custom:` beside each one:

```yaml
# interfaces/adt-to-lab/interface.yaml, in part
custom:
  baa_signed: "yes"
  change_ticket: CHG-1234
  analyst: ana@hospital.org
  runbook: https://wiki.hospital.org/interfaces/adt-to-lab
  go_live: 2026-11-15
destinations:
  lab:
    mllp: { host: ${LAB_HOST}, port: ${LAB_PORT} }
    custom: { analyst: ben@hospital.org }
```

```yaml
# resources.yaml, in part
sftp:
  quest:
    host: ${QUEST_SFTP_HOST}
    user: hospital
    private_key: ${secret:QUEST_SFTP_KEY}
    custom: { contract: QUEST-2025 }
```

([`custom`](../reference/config/interface.md#custom) on interfaces; the same key on destinations
and on every resources.yaml entry.)

## What to know

- **Checked by `ductus validate`, not by the engine**: a wrong value is a validate error and
  nothing else ([nuance](../nuances.md#your-own-documentation-fields-are-checked-by-ductus-validate-not-by-the-engine-level-2)).
  Run validate in CI and the fields stay filled.
- **The web UI shows them as of the engine's last start**: it keeps a snapshot from the project
  it started with ([nuance](../nuances.md#the-documentation-snapshot-is-as-old-as-the-last-engine-start-level-3)).
- **Read-only in the web UI**: a change is a commit, reviewed like any other.
- Keys are yours to choose, except names Ductus already uses beside `custom` (`name`, `owner`,
  `vendor`, …).
