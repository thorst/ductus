# The Ductus engine, built once, as a container image and as Linux packages.
#   docker build -t ductus:dev .                                         the image
#   docker build --target packages --build-arg VERSION=0.6.0-dev.20261028 --output dist .
#                                              .deb, .rpm and the portable .tar.gz into dist/
# Development builds until v1.0: the project (interfaces/, ductus.yaml, ...) is mounted at
# /project, and Postgres runs elsewhere (packaging/docker/compose.yaml). See packaging/README.md.

# The web UI, embedded in the binary.
FROM node:24-trixie-slim AS ui
WORKDIR /src/ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run build

# The engine, built on Rocky Linux 9: it then needs glibc 2.34 or later, so the same binary runs
# on RHEL 9 and its rebuilds, Ubuntu 22.04, Debian 12 and later, and in the image below (a binary
# built on a newer system wouldn't start on older ones).
FROM quay.io/rockylinux/rockylinux:9 AS engine
RUN dnf install -y --setopt=install_weak_deps=False gcc gcc-c++ make perl python3 \
    && dnf clean all \
    && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH=/root/.cargo/bin:$PATH DUCTUS_REQUIRE_UI=1 CARGO_TARGET_DIR=/target
WORKDIR /src
COPY . .
COPY --from=ui /src/ui/dist ui/dist
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/target \
    cargo build --release --locked --bin ductus && cp /target/release/ductus /ductus

# The .deb and .rpm (nfpm, packaging/linux/nfpm.yaml).
FROM ghcr.io/goreleaser/nfpm:latest AS package-build
ARG VERSION=0.0.0-dev
WORKDIR /src
COPY packaging/linux packaging/linux
COPY --from=engine /ductus /ductus
ENV ARCH=amd64 VERSION=${VERSION}
RUN mkdir /out \
    && nfpm package --config packaging/linux/nfpm.yaml --packager deb --target /out/ \
    && nfpm package --config packaging/linux/nfpm.yaml --packager rpm --target /out/

# The portable archive (ductus-<version>-linux-x64.tar.gz): the same binary, the license, and the
# unit and settings file as samples; for `ductus upgrade --check` before installing, and servers
# without a package manager's access.
FROM debian:trixie-slim AS archive-build
ARG VERSION=0.0.0-dev
COPY --from=engine /ductus /a/ductus-${VERSION}/ductus
COPY LICENSE packaging/archive/README.md packaging/linux/ductus.service packaging/linux/ductus.env /a/ductus-${VERSION}/
RUN mkdir /out && tar -C /a --owner=0 --group=0 -czf /out/ductus-${VERSION}-linux-x64.tar.gz ductus-${VERSION}

FROM scratch AS packages
COPY --from=package-build /out/ /
COPY --from=archive-build /out/ /

# The image (the default target).
FROM debian:trixie-slim
# CA certificates: TLS to receivers, APIs and databases is verified against the system's.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --home-dir /project --no-create-home --shell /usr/sbin/nologin ductus
COPY --from=engine /ductus /usr/local/bin/ductus
ARG VERSION=dev
LABEL org.opencontainers.image.title="Ductus" \
      org.opencontainers.image.description="Healthcare integration engine (development build: not for production before v1.0)" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.source="https://github.com/thorst/ductus"
USER ductus
WORKDIR /project
# Inside a container the API must listen on every address to be reachable through a published
# port; publish it to 127.0.0.1 on the host unless a TLS proxy is in front.
ENV DUCTUS_PROJECT=/project DUCTUS_API_LISTEN=0.0.0.0:7700
EXPOSE 7700
# The engine stops cleanly on SIGTERM within DUCTUS_STOP_TIMEOUT (30 s): give `docker stop` longer
# than its default 10 s (`--time 45`, or `stop_grace_period` in compose).
STOPSIGNAL SIGTERM
ENTRYPOINT ["ductus"]
CMD ["run"]
