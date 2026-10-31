# About and owners

[`about`](../reference/config/interface.md#about) says what an interface is for, who owns it, and
the systems on each side. **Nothing in it changes how messages flow**: it's what people and tools
read when something goes wrong.

```yaml
# interfaces/adt-to-lab/interface.yaml, in part
about:
  description: ADT from Epic to the Sunquest lab
  owner: lab-it@hospital.org
  from:
    application: Epic
    vendor: Epic Systems
    hosts: [10.20.30.0/24, epic-bridge01.hospital.org]
  to:
    application: Sunquest Lab
    vendor: Sunquest
```

## Why fill it in

- **`ductus impact`** answers "what does this touch?": `ductus impact --vendor "Epic Systems"`
  for a vendor's outage or breach, `ductus impact --host 10.20.30.0/24` for a network change, with how
  each interface is affected (it sends to them, receives from them, queries their database) and
  who owns it. It knows only what's named
  ([nuance](../nuances.md#ductus-impact-only-knows-whats-named-level-2)): the application and
  vendor on each side, the hosts senders connect from, and the same on resources.yaml entries.
- **On call**, the web UI shows the description and owner on the interface's page, and alert
  rules can send to the owner: the person woken at 3 a.m. knows what broke and who to call.
- **`hosts`** is the one thing the engine can't learn from the configuration (who connects to
  it): with it, a firewall change can be traced to the interfaces it affects, and
  `ductus validate` warns when the source's `allow:` list would refuse a host named here.

`ductus validate` warns about an interface that doesn't name the application or vendor on each
side; [`require_about: true`](ductus-yaml.md#insisting-on-documentation) in ductus.yaml makes it
an error, once your existing interfaces are documented.

## Owners

`about.owner` is who to ask about the interface: a person or a team. **Write it as an email
address** (`lab-it@hospital.org`): an address matches the person's account (by its email) and
survives a change of name, where a name might match two people or nobody.

- **A person with an account** shows as that person in the web UI, and their Interfaces page
  opens on what they own ("Mine"). With a directory, people who own something get their account
  before they first sign in, when they're in a Ductus group
  ([nuance](../nuances.md#directory-owners-get-their-account-when-theyre-in-a-ductus-group-level-2)).
- **A team without an account** is reached through `owners:` in ductus.yaml, by the name the
  interfaces give:

  ```yaml
  # ductus.yaml, in part
  owners:
    Lab Systems:
      email: lab-systems@hospital.org
  ```

- **A destination with another owner** (its code or its receiver is another team's) says so on
  the destination: `owner: billing-it@hospital.org`. Otherwise a destination's owner is its
  interface's ([nuance](../nuances.md#a-destinations-owner-is-its-interfaces-unless-it-says-otherwise-level-1)).
- **resources.yaml entries** have an `owner` too: who renews a certificate, who looks after a
  database.

Alert rules with `to: owner` mail whoever owns the interface or destination
([Alerts and owners](alerts.md)). `ductus owners` lists who owns what; with the database it marks
which owners are people with an account, and `--check` fails on an owner that reaches nobody
(run it in CI). When someone leaves, `ductus owners reassign --from old@hospital.org --to
new@hospital.org` moves everything they owned, in interface.yaml and resources.yaml alike: a
change to commit, like any other
([nuance](../nuances.md#moving-an-owner-moves-their-resourcesyaml-entries-too-level-2)).

## Your own fields

[`custom`](../reference/config/interface.md#custom), beside `about`, holds the documentation your
company keeps that Ductus doesn't ask for (a BAA reference, the change ticket, the analyst, a
runbook link): [Documentation fields](documentation-fields.md).
