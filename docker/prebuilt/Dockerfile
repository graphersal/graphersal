# syntax=docker/dockerfile:1.7
#
# A self-contained knowledge base image: a prebuilt static `graphersal` binary, a Store
# (`graph.gstore`) and an MCP configuration baked in, served READ-ONLY with a read-only MCP
# endpoint. Nothing is compiled: the binary comes from a GitHub Release (or a run of the "CLI
# binaries" workflow) through the local cache of docker/prebuilt/fetch-binary.sh, the store from
# docker/prebuilt/prepare-kb.sh. Both come in as BuildKit named contexts, so neither is copied
# into a build context (book: "Running in Docker", section "A knowledge base image"):
#
#   bin=$(docker/prebuilt/fetch-binary.sh v<version>)
#   docker/prebuilt/prepare-kb.sh my.gstore kb-out --config mcp-config.json --bin "$bin"
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     --build-context bin="$bin" --build-context kb=kb-out \
#     --build-arg GRAPHERSAL_VERSION="$(cat "$bin/VERSION")" \
#     --build-arg GRAPHERSAL_SOURCE="$(cat "$bin/SOURCE")" \
#     --build-arg STORE_FORMAT_VERSION="$(cat kb-out/store-format-version)" \
#     -f docker/prebuilt/Dockerfile -t my-kb:<tag> docker/prebuilt
#   docker run --rm --read-only -p 127.0.0.1:8080:8080 my-kb:<tag>
#
# Context `bin`: linux-amd64/graphersal and linux-arm64/graphersal (the platforms built).
# Context `kb`: graph.gstore, mcp-config.json and the instructions file the config names (*.md
# or *.txt), as prepare-kb.sh writes them.
#
# NO AUTHENTICATION: whoever reaches the port reads the whole knowledge base. Keep the port on
# a private network or behind a reverse proxy (TLS, idle timeout, connection cap; its public
# names go into GRAPHERSAL_ALLOWED_HOSTS).

FROM gcr.io/distroless/static-debian12:nonroot

ARG TARGETARCH
ARG GRAPHERSAL_VERSION=unknown
ARG GRAPHERSAL_SOURCE=unknown
ARG STORE_FORMAT_VERSION=unknown
ARG BUILD_TIME=unknown

LABEL org.opencontainers.image.title="graphersal knowledge base" \
      org.opencontainers.image.description="A Graphersal Store served read-only with a read-only MCP endpoint" \
      org.opencontainers.image.source="https://github.com/graphersal/graphersal" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.created="${BUILD_TIME}" \
      dev.graphersal.binary.version="${GRAPHERSAL_VERSION}" \
      dev.graphersal.binary.source="${GRAPHERSAL_SOURCE}" \
      dev.graphersal.store.format-version="${STORE_FORMAT_VERSION}"

# Layers from rarely to often changed: binary, configuration, knowledge base (a knowledge base
# update rebuilds and ships only the last layer). Everything belongs to root, so the server
# (uid 65532) can read but not write it; it writes nothing anyway (read-only serving mode).
# (No --chmod on the kb files: it would also apply to the /kb directory it creates.)
COPY --from=bin --chmod=0555 linux-${TARGETARCH}/graphersal /usr/local/bin/graphersal
COPY --from=kb mcp-config.json *.md *.txt /kb/
COPY --from=kb graph.gstore /kb/graph.gstore

USER 65532:65532
WORKDIR /kb
EXPOSE 8080
# SIGTERM drains: /readyz turns 503, running queries get --drain-timeout (20 s) to finish, then
# the process exits 0. Give the container a grace period above it (docker stop -t 30).
STOPSIGNAL SIGTERM
# Readiness through the binary itself (no curl in the image): GET /readyz on 127.0.0.1:8080,
# healthy once the store and its embedding model are loaded. The start period covers that load:
# about 4 s for a 565 MiB store with a 506 MiB model on a laptop (book); raise it for a larger
# knowledge base (--build-arg does not reach HEALTHCHECK: docker run --health-start-period).
HEALTHCHECK --interval=10s --timeout=3s --start-period=60s --retries=3 \
    CMD ["/usr/local/bin/graphersal", "health", "--ready"]
ENTRYPOINT ["/usr/local/bin/graphersal"]
# The read-only serving mode with the operator's MCP configuration: no token, no authentication.
# Add arguments with `docker run ... my-kb:<tag> <args>` only by repeating this whole command.
CMD ["--graph", "/kb/graph.gstore", "--read-only", "--server", "--host", "0.0.0.0", "--port", "8080", "--mcp", "--mcp-read-only", "--mcp-config", "/kb/mcp-config.json"]
