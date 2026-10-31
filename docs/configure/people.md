# People: users, tokens, the directory

Everyone who uses the web UI or the REST API is a **person with an account** (who signs in with a
password, theirs or the directory's), or a **token** (for scripts and monitoring). Both have a
role, and maybe extra permissions; everything either does that changes something is in the
audit log, by name.

## Roles and permissions

| Role | Can |
|---|---|
| `viewer` | See interfaces, health, queues, messages' status and errors, alerts. Change nothing. |
| `operator` | What viewers can, and act: resend, replay, edit and dismiss messages, pause and resume, drain, run connection checks. |
| `admin` | What operators can, and manage accounts and tokens; has every permission. |

Two **permissions** go on top of a role, given only to the people who need them:

- **`payloads`**: read message contents, which is usually patient data (and edit them, as an
  operator). Without it the message page shows status, destinations and errors, and says the
  payload is hidden. Every read is in the audit log, written before the payload is shown
  ([nuance](../nuances/people.md#reading-payloads-is-a-permission-not-part-of-a-role-level-4)).
- **`audit`**: read and export the audit log. For compliance and security staff, not operators
  ([nuance](../nuances/people.md#the-audit-log-records-who-did-what-and-what-it-leaves-out-level-2)).

## Accounts

Local accounts are made on the server, with the command line (it works on the database
directly, so it's also how the first admin is made, and how you get back in if locked out):

```sh
ductus user create ana --role admin --email ana@hospital.org --display-name "Ana Silva"
ductus user create ben --role operator --permission payloads --email ben@hospital.org --must-change
ductus user list
```

The password is asked for twice (`--password-stdin` for scripts); `--must-change` makes the
person choose their own at the first sign-in. Admins also manage accounts on the web UI's Users
page. **Give everyone an `--email`**: that's how owner texts (`about.owner: ana@hospital.org`) and
alerts reach them.

- **Passwords** are at least 12 characters (`auth.password.min_length`, never below 8), can't be
  the username, one character repeated or a common one; there's no rule for mixing kinds of
  characters (length matters more). Stored as argon2id hashes.
- **Lockout**: five wrong passwords for one username within 15 minutes lock it for 15 minutes
  (`auth.lockout`); `ductus user unlock ben` or the Users page unlocks it at once
  ([nuance](../nuances/people.md#passwords-lockout-and-what-a-disabled-account-loses-level-2)).
- **Disable** (`ductus user disable ben`, or the Users page) ends their sessions at once and stops
  every token of theirs. Accounts are never deleted: their history in the audit log stays theirs.
  The last admin can't be disabled or demoted.
- **Sessions** in the browser last 12 hours unused and 7 days at most (`auth.session`).

```yaml
# ductus.yaml, in part
auth:
  session: { idle: 8h, max: 1d }
  password: { min_length: 14 }
  lockout: { attempts: 5, window: 15m, for: 15m }
  token_sign_in: false
  user_tokens: true
```

All of it is under [`auth`](../reference/config/ductus.md#auth) in ductus.yaml; the defaults suit
most sites. Shorten `session.idle` where screens are shared or left unlocked.

## Tokens

A token is for a script, a CI job or monitoring: `Authorization: Bearer dux_…` on every call.

```sh
ductus token create prometheus --role viewer            # printed once; stored only as a hash
ductus token create nightly-replay --role operator --permission payloads
ductus token list
ductus token revoke prometheus
```

- **People can make their own** under Account in the web UI (`auth.user_tokens`, on by default):
  a person's token can do no more than they can, and stops when their account is disabled.
- **A token has no permissions** unless it's given them: a monitoring token needs none.
- **`auth.token_sign_in`** (on by default) lets the sign-in page take a token instead of a
  password: how the first people get in before accounts exist. **Turn it off** once everyone has
  an account, so a leaked script token can't also open the web UI.

## Signing in with the directory

With a directory (Active Directory, OpenLDAP, FreeIPA), people sign in with their usual username
and password, and the directory's groups decide their role. Two pieces: the directory itself in
resources.yaml, and how it's used in ductus.yaml.

```yaml
# resources.yaml, in part
ldap:
  corp-ad:
    url: "ldaps://hospital.org"          # the domain's name reaches any controller
    bind_dn: "CN=svc-ductus,OU=Service Accounts,DC=hospital,DC=org"
    bind_password: ${secret:LDAP_BIND_PASSWORD}
    ca: hospital-ca                      # the domain's CA, where this server doesn't trust it
```

```yaml
# ductus.yaml, in part
auth:
  ldap:
    resource: corp-ad
    user_base: "OU=Staff,DC=hospital,DC=org"
    groups:
      admin: "CN=Ductus Admins,OU=Groups,DC=hospital,DC=org"
      operator:
        - "CN=Interface Team,OU=Groups,DC=hospital,DC=org"
        - "CN=Night Operators,OU=Groups,DC=hospital,DC=org"
      viewer: "CN=Help Desk,OU=Groups,DC=hospital,DC=org"
      permissions:
        payloads: "CN=Ductus PHI,OU=Groups,DC=hospital,DC=org"
        audit: "CN=Ductus Auditors,OU=Groups,DC=hospital,DC=org"
    refresh: 15m
```

- **The directory entry** ([`ldap`](../reference/config/resources.md#ldap)): `ldaps://` (port
  636), or `ldap://` with `starttls: true`. Plain `ldap://` is refused unless `insecure: true`,
  which is only for a test directory on the same machine: passwords would cross the network
  readable ([nuance](../nuances/people.md#the-directory-connection-is-tls-verified-like-the-rest-level-3)).
  The service account (`bind_dn`) needs read access to people and groups, nothing else; give it a
  password that doesn't expire, or note when it does: an expired one stops directory sign-ins.
- **Who may sign in**: people under `user_base`, found by `user_filter`
  (`(sAMAccountName={username})` by default; `(uid={username})` for OpenLDAP and FreeIPA), and in
  at least one role group. The highest role their groups give wins; `permissions` groups add to
  it. Someone in no role group is told they have no access
  ([nuance](../nuances/people.md#signing-in-with-the-directory-local-names-first-groups-decide-the-role-level-2)).
- **Write several groups one per line**, as above, never `[a, b]`: DNs have commas
  ([nuance](../nuances/people.md#write-several-group-dns-one-per-line-not-a-b-level-2)).
- **Their account** is made at their first sign-in, marked as a directory account; its role,
  permissions, name and email are the directory's, read-only here. An admin can still disable it.
- **Removal reaches Ductus within `refresh`** (15 minutes by default): someone taken out of the
  groups, disabled or deleted in the directory loses their sessions and tokens at the next
  refresh. For someone who must lose access now, disable them on the Users page first
  ([nuance](../nuances/people.md#a-person-removed-from-the-groups-loses-access-within-refresh-level-2)).
- **A renamed person keeps their account** (found by their directory id, `objectGUID` or
  `entryUUID`), and a sign-in name given to someone new gets a new account
  ([nuance](../nuances/people.md#a-renamed-directory-person-keeps-one-account-a-reused-name-gets-a-new-one-level-2)).
- **Nested groups**: `nested: true` counts members of groups inside the configured ones. Prefer
  direct membership: one search per group, easy to audit
  ([nuance](../nuances/people.md#nested-groups-active-directory-answers-other-directories-are-walked-level-4)).

**Keep one local admin** (`ductus user create … --role admin`), with a long password the on-call
can reach: when the directory is down, directory sign-ins are refused ("the directory can't be
reached right now") but local accounts and open sessions keep working
([nuance](../nuances/people.md#when-the-directory-is-down-level-2)). A name a local account holds is
always checked locally, so the directory can't take it over.

**Active Directory has not been tested against yet** (the tests use OpenLDAP); its
transitive-membership filter for nested groups is unit-tested, and the first pilot with a domain
verifies it.

More on directory accounts:

- **A person deleted and made again in the directory** is a new id there, so a new account here:
  it can't be linked to the old one, which is refused at the next refresh
  ([nuance](../nuances/people.md#a-new-directory-account-for-the-same-person-cant-be-linked-to-the-old-one-level-2)).
- **OpenLDAP** follows a renamed person in their groups only with its `refint` overlay
  ([nuance](../nuances/people.md#openldap-group-membership-follows-a-rename-only-with-refint-level-3)).
- **Wrong passwords count twice**: towards Ductus's lockout and towards the directory's own
  ([nuance](../nuances/people.md#wrong-passwords-count-twice-here-and-in-the-directory-level-3)).
