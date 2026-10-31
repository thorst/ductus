# The guides' example project

The code shown in the docs' "Write interfaces" pages (`docs/write/`) comes from this project: a
TypeScript block on those pages that starts with a file's path (`// interfaces/adt-in/to-lab.ts`)
is a piece of that file here, and a CLI test (`the_guides_code_is_this_project_s`) fails when a
page and its file disagree. The same test runs `ductus test` and `ductus validate` here, so every
example on those pages has run.

| Interface | Shows |
|---|---|
| `adt-in` | Routing by content to two destinations, filtering with a reason, a code table, removing Z segments |
| `adt-to-billing` | A route from another interface, a mapping written as a table, a cached database lookup, JSON out |
| `results-to-fhir` | HL7 ORU in, a FHIR R4 transaction Bundle out, to a FHIR server's API |
| `remits-in` | An X12 835 file read as text, written as CSV |
| `census-in` | A CSV file split into records, each turned into a new HL7 message |
| `orders-xml` | An XML document read, changed and written back |

Shared code lives in `lib/`: `codes.ts` (code tables), `mapping.ts` (field-to-field mappings
from a table), `x12.ts` (reading X12 segments and elements), `empi.ts` (an API lookup, tested in
`adt-to-billing`).

Try it: `ductus test` here (no database needed). `ductus validate` wants the secrets
resources.yaml names: `export DUCTUS_SECRET_KEY=$(ductus secret init)`, then
`ductus secret set BILLING_TOKEN` (and `EMPI_TOKEN`, `FHIR_TOKEN`, `PROVIDER_DB_PASSWORD`).
