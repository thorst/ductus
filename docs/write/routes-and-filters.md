# Routes and filters

Which messages go where. In Ductus a route isn't a separate rule table: **every destination gets
every message, and its own function decides** whether to send it, change it, or drop it. One
interface with three destinations is three independent decisions on the same message.

## Filtering: `null` and `kill`

```ts
// interfaces/adt-in/route.ts, in part
/** The visit events the lab system takes. */
const LAB_EVENTS = new Set(["A01", "A03", "A04", "A08"]);

export function toLab(msg: Message) {
  if (!LAB_EVENTS.has(msg.MSH[9][2])) return null; // not for the lab: filtered, no reason needed
  if (msg.PID[3][1].startsWith("TEST")) return kill("test patient");
```

- **`return null`** filters: this destination never gets the message, and nothing is said about
  why. Use it for the ordinary "not for this receiver" cases, the ones nobody will ask about:
  event types a receiver doesn't take, another facility's patients.
- **`return kill("reason")`** filters with a reason, shown on the message in the web UI and kept
  in the database: "filtered: test patient". Use it for anything someone might later ask about
  ("why didn't the lab get this patient?"): test patients, a duplicate you recognized, a message
  outside a date range.
- **Filtering isn't an error.** A filtered message is finished, raises no alert and is never
  retried. It's kept for the interface's [retention](../configure/keeping-and-watching.md), so
  the answer to "did we get it, and what did we do with it?" is always there.
- **When not to filter:** when the message is *wrong*. A missing MRN, an unknown code, a
  timestamp you can't read: those are for a person, so `throw quarantine("…")`
  ([quarantine](#quarantine-for-a-person-to-look-at)). Filtering a broken message hides it.

A filter on a field's value is written in code, not configuration, so it can say exactly what it
means (`startsWith`, a set of codes, a date compared with today) and be tested with real
messages ([testing](testing.md)).

## Routing by content: one destination per receiver

```yaml
# interfaces/adt-in/interface.yaml, in part
destinations:
  lab:
    mllp: { host: ${LAB_HOST}, port: ${LAB_PORT} }
    transform: toLab
  billing:
    interface: adt-to-billing       # a route: into that interface's queue
    transform: toBilling
```

```ts
// interfaces/adt-in/route.ts, in part
/** Billing wants admissions and discharges, as received. */
export function toBilling(msg: Message) {
  return ["A01", "A03"].includes(msg.MSH[9][2]) ? msg : null;
}
```

Each destination names its function with `transform:`. Both get the message **as it was
received**: `toLab` changing the patient class and removing Z segments changes nothing for
`toBilling` ([nuance](../nuances/destinations.md#each-destination-runs-its-own-function-on-the-original-message-level-2)).
So "route ADT to the lab, and admissions and discharges to billing" is two functions of a few
lines, each tested on its own, and a message can go to both, either or neither.

What each destination has of its own, and why that's the point of routing this way:

- **Its own queue and order.** The lab down for an upgrade holds up only the lab; billing keeps
  receiving ([nuance](../nuances/destinations.md#one-destination-down-holds-up-only-itself-level-2)).
- **Its own outcome.** The lab can quarantine a message billing sent; the message shows both
  ([nuance](../nuances/destinations.md#filtering-and-quarantine-are-per-destination-level-2)).
- **Its own pause, retries, rate limit and alerts** ([delivery](../configure/delivery.md)).

**When to split instead.** Past a handful of destinations, or when destinations belong to
different teams who change them on different days, give each receiver an interface of its own
and route to it ([below](#to-another-interface)): each then has its own code, owner, tests and
deploys ([when to split](../nuances/destinations.md#many-destinations-on-one-interface-when-to-split-level-3)).

## To another interface

```yaml
# interfaces/adt-to-billing/interface.yaml
name: adt-to-billing
about:
  description: Admissions and discharges from adt-in, as JSON encounters for the billing system
  owner: revenue-cycle-team
  to: { application: Billing, vendor: Experian }
source: internal                    # messages come only from another interface's route
script: billing.ts
ordering: { key: PID-3.1 }
output: json
destination:
  http: { api: billing, path: /encounters }
```

A destination with `interface: adt-to-billing` puts the message into that interface's queue, in
the same database transaction that marks it sent from here. Nothing crosses a network, and
nothing is lost or sent twice between the two
([nuance](../nuances/routes.md#a-route-is-exactly-once-inside-the-engine-not-beyond-it-level-3)).
The receiving interface has `source: internal` and is an interface like any other: its own code,
ordering key, dedupe, destinations, retries, owner and alerts
([nuance](../nuances/routes.md#the-receiving-interfaces-keys-dedupe-and-destinations-apply-level-2)).
Its code sees `meta.origin`: the message's id in the interface it came from.

Use a route to:

- **Hand a feed to another team.** The integration team owns `adt-in` (the connection to the EHR,
  the ACKs, the dedupe); revenue cycle owns `adt-to-billing`, its mapping and its alerts.
- **Chain steps** that change at different speeds: a normalizing interface every downstream
  interface routes from, so a change of the EHR's quirks is made once.
- **Fan one feed out to many receivers** without one interface carrying a dozen destinations.

What to know:

- **The route is fixed in interface.yaml.** Code can't pick an interface at run time; it decides
  only *whether* this destination sends (`null` or the message). That's what lets
  `ductus validate` refuse a loop and check that the receiving interface reads the format this
  one writes, and `ductus impact` follow a vendor's messages through every hop
  ([nuance](../nuances/routes.md#routes-are-fixed-in-interfaceyaml-and-loops-are-refused-level-2)).
- **Order holds across a route** when the sending interface keeps it (an ordering key), and the
  receiving one has a key too ([nuance](../nuances/routes.md#order-holds-across-a-route-when-the-sender-keeps-it-level-4)).
- **A routed message waits up to 20 ms** before the next interface sees it: nothing a feed
  notices ([nuance](../nuances/routes.md#a-routed-message-waits-up-to-20-ms-before-the-next-interface-sees-it-level-4)).

## Quarantine: for a person to look at

```ts
// lib/codes.ts, in part
export function translate(table: Readonly<Record<string, string>>, code: string, what: string): string {
  const value = table[code];
  if (value === undefined) throw quarantine(`unknown ${what} "${code}"`);
  return value;
}
```

`throw quarantine("reason")` holds this destination's delivery of the message for a person: it
shows on the Quarantine page and in the interface's error count, alerts when your rules name
`error`, and waits until someone resends it (after fixing the code or the table), corrects and
resends it, or dismisses it ([resend and correct](../operate/resend.md)). Write the reason for
the person who will read it at 3 a.m.: what's wrong and with which value ("unknown patient class
\"X\""), not "error".

Any other error your code throws (a `TypeError` from a field that isn't there, a failed
`JSON.parse`) quarantines the message too, with the error's text. Prefer `quarantine()` for
every case you can foresee: its reason is yours, not a stack trace.

**Quarantine doesn't stop the queue.** The next message for that destination goes on, so one bad
message doesn't hold up the feed. Messages that wait instead, in order, are the ones that hit a
*temporary* failure: a receiver or database that's down. Those are tried again until they go
([nuance](../nuances/ordering.md#temporary-failures-retry-forever-by-default-level-3)).

## Routing by sender or event without code

Two cases often handled by routing rules elsewhere need none here:

- **Different senders on different ports**: give each sender its own interface (its own
  `source`), so each has its own `about.from`, `allow:` list and alerts. Two senders on one port
  can't be told apart reliably by content.
- **A repeat of a message already received**: [`dedupe`](../reference/config/interface.md#dedupe)
  on `MSH-10` recognizes it before any code runs.
