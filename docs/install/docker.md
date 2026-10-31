# Docker

The image runs the same engine as the packages, in a container: Debian 13 slim, the binary at
`/usr/local/bin/ductus`, the system's CA certificates, running as user `ductus` (uid 10001),
never root. The project is mounted read-only at `/project`.

**Use it** to try Ductus with its database in one command, and to develop with containers.
**Don't** use it for production before v1.0: the image is a development build, it isn't
published to a registry yet (you build it), and a container's restart refuses senders for a
moment, where the Linux package's service refuses none
([nuance](../nuances/upgrades.md#windows-and-containers-a-restart-is-a-short-gap-level-2)). From v1.0,
production images will be built `FROM` the engine's image with the project inside: one image per
release of your interfaces. Kubernetes, with rolling upgrades, is planned for v1.2.

## Try it with Compose

From a clone of the repository:

```sh
docker build -t ductus:dev .
PROJECT=$PWD/examples/adt-relay docker compose -f packaging/docker/compose.yaml up
```

That starts Postgres 18 and the engine on the example project. Then, in a second terminal, make
yourself an account (the database is the container's, so the command runs there):

```sh
docker compose -f packaging/docker/compose.yaml exec engine ductus user create me --role admin
```

and sign in to the web UI at `http://127.0.0.1:7700`.

The Compose file is short and worth reading before you change it:

```yaml
# compose.yaml (packaging/docker/compose.yaml, in part)
services:
  engine:
    image: ${IMAGE:-ductus:dev}
    environment:
      DUCTUS_DATABASE_URL: postgres://ductus:${POSTGRES_PASSWORD:-ductus}@postgres/ductus
      DUCTUS_ENV: ${DUCTUS_ENV:-dev}
      DUCTUS_SECRET_KEY: ${DUCTUS_SECRET_KEY:-}
    volumes:
      - ${PROJECT:?set PROJECT to your project folder}:/project:ro
    ports:
      - 127.0.0.1:7700:7700   # the API and web UI: this machine only
      - 6661:6661             # each interface's listener
    stop_grace_period: 45s
```

## What to know

- **Settings are environment variables**, the same as everywhere
  ([the list](index.md#what-every-install-has-in-common)). Keep `DUCTUS_SECRET_KEY` and the
  database password out of the Compose file: Compose reads `${...}` from a `.env` file beside it
  (one you don't commit), and an orchestrator can pass its secrets in as environment variables.
  The engine reads them only as variables, not as files.
- **The API listens on `0.0.0.0:7700` inside the container**, so Docker can publish it: the
  image sets `DUCTUS_API_LISTEN=0.0.0.0:7700`, which wins over `api.listen` in ductus.yaml
  (set the variable to move it). Publish
  it to `127.0.0.1` (as above) unless the API has [`tls:`](../configure/tls.md#the-web-ui-and-the-api)
  or a TLS proxy is in front: the web UI's sign-in needs HTTPS anywhere but on the machine itself.
- **Publish each interface's listener** (`6661:6661` for an MLLP port). `ductus ports` lists
  every port a project listens on.
- **Give a stop time.** Docker waits 10 s by default before it kills a container; a clean stop
  can need up to `DUCTUS_STOP_TIMEOUT` (30 s). `stop_grace_period: 45s` (or
  `docker stop --time 45`) lets the engine finish the messages in hand
  ([what finishes](../nuances/delivery.md#stopping-the-engine-what-finishes-and-what-waits-level-2)).
- **The project is read-only**: interfaces change through Git and a new container, never inside
  a running one. A file destination needs a volume of its own, writable by uid 10001.
- **The database** in the Compose file is for trying it out. For anything that matters, point
  `DUCTUS_DATABASE_URL` at the DBA's Postgres ([PostgreSQL](postgres.md)).
- **Time zone**: the image runs on UTC unless you set `TZ`. Cron schedules, quiet hours and
  `monitor:` times are read in the container's local time.
