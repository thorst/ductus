# Nuances: SQL Server Windows logins

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## `auth: windows`: the engine's own account, or a named one (level 1)
- **What:** `auth: windows` with no `user` logs in as the account the engine runs as (integrated, like `Integrated Security=true`); with `user: 'HOSPITAL\svc-lab'` and its `password` it logs in as that account. `auth: sql` (the default) is a SQL login. The account the engine runs as exists only on Windows: on Linux, `auth: windows` without a `user` is refused when resources.yaml loads.
- **Why:** a SQL Server that allows only Windows logins (many hospital DBAs set it that way) needs one of the two; integrated keeps a password out of the configuration altogether.
- **What to do:** on Windows prefer integrated, with the service running as a gMSA (no password anywhere). Ask the DBA for a login for that account: `CREATE LOGIN [HOSPITAL\svc-ductus$] FROM WINDOWS`, then a user in the database.

## The service's own account is the computer on the network (level 3)
- **What:** the Windows service runs as `NT SERVICE\Ductus` by default. On the network that account is the computer's: `HOSPITAL\SERVER01$`. An integrated login from the default service is therefore a login for `HOSPITAL\SERVER01$`, and anything else on that server running as a service account gets the same identity.
- **Why:** a virtual service account has no network identity of its own; Windows uses the machine account for it.
- **What to do:** for a dedicated identity, run the service as a gMSA (`HOSPITAL\svc-ductus$`: `ductus service account 'HOSPITAL\svc-ductus$'`); the start log and doctor say which account SQL Server saw ("SQL Server logged us in as HOSPITAL\SERVER01$"). `ductus validate` and `ductus doctor` on the server say which account the service runs as and warn when an integrated login would be the computer account's. Doctor's own integrated logins are yours (it runs in your terminal), not the service's: it says so.

## Kerberos or NTLM: doctor and the start log say which (level 3)
- **What:** on Windows the login asks for Kerberos and falls back to NTLM by itself (SSPI's Negotiate, as ODBC and ADO.NET do). Doctor's login step and the start log (once per SQL Server, for Windows logins) say who the server took the login for and how: "(Windows, Kerberos)" or "(Windows, NTLM: no Kerberos, most likely no SPN for MSSQLSvc/sql1.hospital.org:1433)". NTLM to another machine is a warning in doctor; NTLM to a SQL Server on the same machine is usual and passes.
- **Why:** Kerberos needs the SQL Server's SPN (`MSSQLSvc/host:port`, or `:INSTANCE`) registered for the account SQL Server runs as, and the engine connecting by that name (not an IP address or an alias). Without it the login still works, as NTLM, and nothing else tells you.
- **What to do:** if doctor says NTLM, ask the DBA to register the SPN (`setspn -S MSSQLSvc/sql1.hospital.org:1433 HOSPITAL\sqlsvc`) and connect by the name in it. Kerberos itself has not been tested against a domain yet (no domain in our test setups): the first site with one confirms it.

## A refused Windows login: 18452 is Windows saying no, 18456 is SQL Server (level 2)
- **What:** when Windows doesn't accept the account, SQL Server answers error 18452, "the login is from an untrusted domain", whatever the cause: a wrong password, an account the server's Windows doesn't know, or NTLM blocked by the domain. When Windows accepts it but SQL Server has no login for it (or the login is disabled, or can't open its default database), the answer is 18456, "Login failed for user". The engine words both, and, as with a refused SQL login, messages wait (a temporary error) until it's fixed.
- **Why:** the server doesn't tell the client more; its own error log does.
- **What to do:** 18452: check the password secret for this environment and the account's name, then whether the domain restricts NTLM (or get Kerberos working: the SPN). 18456: ask the DBA for `CREATE LOGIN [DOMAIN\name] FROM WINDOWS` and a user in the database. A login given to a Windows group (SQL Server Express gives one to `BUILTIN\Users`, which every signed-in account is in) lets every member in: an account can get in with no login of its own.

## From Linux, a Windows login is always NTLM (level 3)
- **What:** on Linux, `auth: windows` needs a named account and logs in with NTLM, in pure Rust: never Kerberos. Validate and the start warn about it. Microsoft has deprecated NTLM (2024), many security teams audit it, and a domain that blocks NTLM refuses these logins (18452).
- **Why:** Kerberos from Linux needs a keytab and the system's GSSAPI library, which the binary would then need on every server whether it's used or not; it waits for the first site that needs it.
- **What to do:** from Linux prefer a SQL login, or run the engine on Windows for Windows logins. If you use NTLM, tell the security team, and expect it to stop working when the domain turns NTLM off.

## Extended Protection set to Required refuses NTLM logins (level 3)
- **What:** a SQL Server with Extended Protection for Authentication set to Required refuses an NTLM login that carries no channel binding, and the engine's carries none. Kerberos logins are not refused for it.
- **Why:** channel binding ties the login to the TLS connection; the library the engine uses doesn't send one yet (a possible later addition). Not tried here: our test server leaves Extended Protection off, as SQL Server does by default.
- **What to do:** use Kerberos (the SPN), Extended Protection set to Allowed, or a SQL login.
