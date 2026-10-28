# syntax=docker/dockerfile:1.7
#
# The `graphersal` command line as a container image: a static musl binary (the build of the
# Linux release binaries of .github/workflows/binaries.yml: crate graphersal-cli, default
# features incl. the dev server, MCP, the AI Chat and Model2Vec embedding models, profile
# `dist`, thin LTO unless --build-arg LTO=fat) on distroless/static, as a non-root user.
#
#   docker build -t graphersal .
#   docker run --rm -p 8080:8080 -v "$PWD/graph.gstore:/data/graph.gstore:ro" graphersal
#
# The default command serves /data/graph.gstore READ-ONLY with a read-only MCP endpoint
# (book: "Running in Docker"). The dev server has NO authentication: keep the port on a private
# network or behind a reverse proxy that authenticates.
#
# The base image versions are examples: bump them (and RUST_VERSION, at least the MSRV 1.88)
# when you build.

ARG RUST_VERSION=1.88
ARG ALPINE_VERSION=3.22

# --- build: a static binary (musl is static by default on Alpine) ----------------------------
FROM rust:${RUST_VERSION}-alpine${ALPINE_VERSION} AS build
# ring (TLS for the AI Chat and model downloads, SHA-256) compiles C code.
RUN apk add --no-cache musl-dev
WORKDIR /src
# The release binaries link with fat LTO, which needs more than the 8 GB of a default Docker
# Desktop VM; thin LTO builds in less memory and runs within a few percent of it.
# `--build-arg LTO=fat` gives the release build exactly.
ARG LTO=thin
ENV CARGO_PROFILE_DIST_LTO=${LTO}
COPY . .
# The registry, git checkouts and the target directory live in BuildKit cache mounts: a source
# change rebuilds only the workspace crates, never the dependencies, and no build artefact
# lands in an image layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --locked -p graphersal-cli --profile dist \
    && install -D -m 0755 target/dist/graphersal /out/graphersal \
    && mkdir -p /out/data

# --- runtime ---------------------------------------------------------------------------------
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/graphersal /usr/local/bin/graphersal
# /data: where a Store or graph file is mounted or copied (owned by the nonroot user 65532, so
# a named volume mounted there is writable for a read-write Store).
COPY --from=build --chown=65532:65532 /out/data /data
USER 65532:65532
WORKDIR /data
EXPOSE 8080
# The server stops on SIGTERM (it handles it as PID 1): the running query is cancelled and a
# read-write Store is closed cleanly.
STOPSIGNAL SIGTERM
# Liveness through the binary itself (no curl in the image): GET /healthz on 127.0.0.1:8080.
# The start period covers loading a large Store; /healthz answers during the load already.
HEALTHCHECK --interval=10s --timeout=3s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/graphersal", "health"]
ENTRYPOINT ["/usr/local/bin/graphersal"]
# --host 0.0.0.0 explicitly, whatever runtime starts the image (the server detects Docker,
# Podman and Kubernetes; another runtime would get the loopback default).
CMD ["--graph", "/data/graph.gstore", "--read-only", "--server", "--host", "0.0.0.0", "--port", "8080", "--mcp", "--mcp-read-only"]
