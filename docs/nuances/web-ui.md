# Nuances: the web UI

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## The UI works over https, or at localhost on the engine's own server (level 4)
- **What:** the engine serves the web UI on the API's listener (`http://127.0.0.1:7700/` by default). Signing in trades a token for a session cookie marked `Secure`, which browsers keep only over https, or over plain http to `localhost`/`127.0.0.1`. Over plain http to another machine the browser drops it, and the sign-in page says so.
- **Why:** the cookie is as good as the token while it lasts; sent in clear text over a hospital network, anyone on the path could take it.
- **What to do:** to reach the UI from other machines, give the API `tls:` in ductus.yaml (`api: { listen: 0.0.0.0:7700, tls: { certificate: ductus-api } }`) and open `https://`; or put a TLS reverse proxy in front (keep `api.listen` on `127.0.0.1` behind it), or use an SSH tunnel (`ssh -L 7700:127.0.0.1:7700 server`, then `http://localhost:7700`).

## A console session acts as whoever signed in (level 2)
- **What:** signing in to the console (a username and password, a directory account, or a token where `auth.token_sign_in` allows it) starts a session that acts with that person's role and permissions, or the token's, and the audit log names them. It ends after 12 hours unused or 7 days after sign-in (`auth.session`), on sign-out, and at once when the account is disabled (or, for a token sign-in, the token revoked). Changes made through the session cookie (pausing, draining) must carry the `X-Ductus: 1` header, which the console sends and other websites can't, so a page elsewhere can't act through someone's session. Scripts use a token and don't need the header.
- **Why:** a session is a person at a browser; it shouldn't outlive their access, and another site open in the same browser mustn't be able to use it.
- **What to do:** give each person an account (or a directory sign-in), not a shared token, so the audit log says who looked at which message or paused what. Shorten `auth.session.idle` where screens are shared or left unlocked.

## Your own colors can be unreadable on purpose (level 1)
- **What:** Appearance (user menu → Customize colors…) takes five colors (header, accent, page background, panel, text) and works the rest out from them. It checks every pair the screens draw against WCAG AA (4.5:1 for text, 3:1 for the focus ring) and marks the ones below it "Hard to read". It warns and does not stop you: **Use these colors** works either way, and the pages are then as hard to read as the check said. The built-in High contrast theme is the one that promises AAA (7:1).
- **Why:** it is your screen and your eyes; a rule that refuses a color nobody else sees would be in the way. But people spend all day in this, so the check is always shown.
- **What to do:** press **Fix it** on a row (or **Fix all**): it moves only the text or the accent, in lightness and just far enough, and shows the old and new values with Undo. It leaves the header, page background and panel alone, unless no text color could be read on them at all (a black page with a white panel): then it pulls the panel toward the page background and says so. The colors are kept in this browser, not on the server, until logins arrive (v0.5 step 8): another browser or machine starts from the system theme.
