# How the docs are written

For anyone changing Ductus or its docs. This file isn't part of the book; the book is everything
listed in `SUMMARY.md`, built with mdBook (`scripts/dev/docs-check.sh` builds it and checks every
link and anchor).

## The rule: docs change with the behaviour

A change to a setting, a command, an API operation or a behaviour someone could notice ships
with its docs **in the same commit**: the setting's doc comment (it is the reference text), the
guide page that explains it, and a nuance when it could surprise someone (`nuances.md`, with a
level). A commit that changes what Ductus does and leaves a page saying the old thing is a bug,
the same as a failing test. The tests below catch some of it; the rest is review.

## The bar for every page

Every setting, command and page answers five questions, in words someone running a hospital's
interfaces would use:

1. **What** it does, in one or two sentences.
2. **Why** you'd use it: the problem it solves.
3. **When not** to: the case where it's the wrong tool, and what to use instead.
4. **The default**, and what happens when you say nothing.
5. **An example** that works as written.

Err toward more. A page that explains why is worth more than one that lists what. If you can't
say when not to use something, find out before you write it.

## Kinds of pages

| Kind | Where | Written by | What it does |
|---|---|---|---|
| Guide | `install/`, `configure/`, the top-level pages | hand | Explains a task start to finish, and why each step is there. Links to the reference for every setting it names. |
| Reference | `reference/` | `ductus reference docs` (from the code) | Lists everything: every command, API operation, setting, Event Log ID, metric and audit action. Never edit by hand: edit the doc comment or the help text it comes from. |
| Nuance | `nuances.md` | hand | What could surprise you, why it is so, what to do; a level 1–5. Guides link to nuances rather than copy them. |
| Architecture | `architecture.md`, `schema.md` | hand | How it works inside, and every table and column. |

## Settings: the doc comment is the reference

A config field's doc comment becomes the setting's description in the JSON Schema (what VS Code
shows on hover) and on its reference page. So it is user-facing text: what, why, when not,
`Default:` and `Example:`, finished sentences, and no Rust (`Option<…>`, a file name, `None`).
`every_setting_is_described_for_people` (ductus-core) fails on a setting without one, or one
that reads as code.

## No orphan setting

Every top-level setting of each configuration file, and each of their connectors (`source.mllp`,
`destination.http`, `databases.<name>.postgres`, `format.csv`), is linked from at least one
guide page that explains it: a link to its reference heading,
`[`retention`](../reference/config/interface.md#retention)`. A new setting can't land with only
its reference entry. `every_setting_is_explained_in_a_guide` (ductus CLI crate) fails and names
each one missing, with the link to write.

## Examples

**YAML examples are checked.** Every fenced `yaml` block in a guide starts with a comment naming
the file it belongs to, and is checked against that file's schema and read the way the engine
reads it (`every_yaml_example_in_the_guides_is_valid`, ductus-core):

````markdown
```yaml
# interfaces/adt-in/interface.yaml
name: adt-in
source: { mllp: { port: 6661 } }
destination: { file: { dir: out } }
```
````

- A whole file: `# resources.yaml`, `# ductus.yaml`, `# environments/prod.yaml`,
  `# docs-schema.yaml`, or any path ending in `interface.yaml`.
- Some of a file's top-level settings: `# interfaces/adt-in/interface.yaml, in part`. The test
  adds what the file needs besides (an interface's `name`, `source` and `destination`). A part
  is always whole top-level settings (`retry:` with what's under it), never a piece from inside one.
- `${NAME}` in an example needs a value: the test takes the variables from the guides'
  `environments/*.yaml` examples, so define it in one (`configure/environments.md` has them).
- YAML that isn't a Ductus file (Docker Compose, a CI workflow) names its file too
  (`# compose.yaml`), and isn't checked.

**TypeScript examples are the guides' example project's.** Every fenced `ts` block on the
"Write interfaces" pages (`docs/write/`) starts with the path of a file in `examples/guides`
(`// interfaces/adt-in/route.ts`, or `// lib/codes.ts, in part` for a piece) and is in that file
word for word; a `ts` block elsewhere in the guides that names such a path is checked too
(`the_guides_code_is_the_example_projects`, CLI crate). The project's own `ductus test` and
`ductus validate` run in the CLI tests (`the_guides_example_project_passes_its_tests_and_validate`),
so a page's code has run. To show new code, add it to the project with a test, then quote it.

**Commands are real.** Every command on a page has been run, and its output, where shown, is
what it printed (trimmed with `…`, never made up). Measurements name what was measured, on what,
and link to where it's recorded (`architecture.md`, a nuance); they're never worded as limits
("measured at", not "handles up to").

**Names in examples** are a hospital's, made up: `hospital.org`, `lab.hospital.org`,
`10.20.0.15`, `Epic`, `Sunquest`. Never a real person; never a real organization's host.

## Words

- Plain words over jargon; say what a thing does before what it's called.
- "You" for the reader; the engine is "the engine" or "Ductus", never "we" for the software.
- Say when someone must act and what to do: "when the queue passes 10 minutes, look at …", not
  "monitor the queue".
- Workloads by their shape (a feed, an API, a nightly batch), not only "hospital volumes":
  people use Ductus outside healthcare too.
- Name the setting as it's written (`retention: 3w`), in backticks, linked to the reference the
  first time a page names it.
- Linux first, Windows next to it where they differ; never Windows as an afterthought.
- Headings are sentences in sentence case; nuance headings say the surprise ("A message can be
  delivered twice"), not the topic.

## Before you commit

`scripts/dev/check.sh` regenerates the reference, runs the tests above (descriptions, no orphan
setting, YAML examples, no mangled characters) and, when mdBook and lychee are installed
(`scripts/dev/docs-tools.sh`), builds the book and checks every link and anchor. `mdbook serve
docs` shows the book at http://localhost:3000 as you write.
