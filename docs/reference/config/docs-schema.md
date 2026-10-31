# `docs-schema.yaml`

`docs-schema.yaml`: the documentation fields your company keeps about its interfaces, destinations and resources, beyond what Ductus asks for (a BAA reference, a change ticket, a runbook link, the analyst). Values go in `custom:` beside each one; `ductus validate` checks them, and the web UI shows and filters by them. The engine itself ignores them.

The file sits at the top of the project and is optional: without it there are no fields.

Schema for editors: `docs/schema/docs-schema.json` (written into a project by `ductus types`). Files: `docs-schema.yaml`.

### `fields`

**Type:** names, each with settings, below

The fields, by the key `custom:` uses, in the order the web UI shows them. Keys are your choice, except names Ductus uses beside `custom` (`name`, `owner`, `vendor`, …). Default: none. Example: `fields: { baa_signed: { type: enum, values: ["yes", "no"], required: true } }`.

### `fields.<name>.type`

**Type:** `text`, `enum`, `date`, `url`, `person`, `list` · **Required**

What the value is:

- `text`: any text (quote numbers and yes/no, or YAML reads them as such).
- `enum`: one of `values`.
- `date`: a date, written `2026-10-30`.
- `url`: an `http(s)://` address, shown as a link.
- `person`: who, as text: best an email address.
- `list`: several texts (each one of `values`, if given).

Required. Example: `type: enum`.

### `fields.<name>.values`

**Type:** list of text

The values an `enum` may take (and each item of a `list`, if given), matched exactly, case included. Quote ones YAML would read otherwise (`"yes"`, `"no"`). Default: none. Example: `values: ["yes", "no", n/a]`.

### `fields.<name>.required`

**Type:** `true` or `false`

Every interface (destination, resource) the field applies to must have a value: `ductus validate` reports each one missing. The engine starts anyway. Default: `false`. Example: `required: true`.

### `fields.<name>.applies_to`

**Type:** list of `interface`, `destination`, `resource`

Where the field may be given: `interface`, `destination` and/or `resource` (a resources.yaml entry). Default: `[interface]`. Example: `applies_to: [interface, resource]`.

### `fields.<name>.label`

**Type:** text

The name the web UI shows instead of the key. Default: the key. Example: `label: BAA signed`.

### `fields.<name>.help`

**Type:** text

A hint shown under the field in the web UI: what it's for, or where to find the value. Default: none. Example: `help: Business associate agreement, from the contracts office`.

