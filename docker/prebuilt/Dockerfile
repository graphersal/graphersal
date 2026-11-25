# syntax=docker/dockerfile:1.7
#
# A self-contained knowledge base image: a prebuilt static `graphersal` binary, one prepared Store
# file and the MCP configuration baked in, served READ-ONLY with a read-only MCP endpoint.
# Nothing is compiled and nothing runs during the build: the store is shipped exactly as given
# (fork, compact and verify it before, e.g. `graphersal store fork` + `store verify`).
# build.py writes the build context (.context/) and runs the build:
#
#   python docker/prebuilt/build.py --store my.gstore -t my-kb:1
#
# Build context (what build.py writes; lay it out by hand to build with plain `docker build`):
#   bin/linux-<amd64|arm64>/graphersal   the binary of every platform built
#   store/graph.gstore                   the prepared single-file Store, shipped unchanged
#   kb/mcp-config.json                   the MCP configuration
#   kb/<instructions>.md                 the instructions file the configuration names (optional)
#
# NO AUTHENTICATION: whoever reaches the port reads the whole knowledge base. Keep the port on a
# private network or behind a reverse proxy (its public names go into GRAPHERSAL_ALLOWED_HOSTS).

ARG BASE=gcr.io/distroless/static-debian12:nonroot
FROM ${BASE}
ARG TARGETARCH
ARG GRAPHERSAL_VERSION=unknown
ARG GRAPHERSAL_SOURCE=unknown
ARG BUILD_TIME=unknown

LABEL org.opencontainers.image.title="graphersal knowledge base" \
      org.opencontainers.image.description="A Graphersal Store served read-only with a read-only MCP endpoint" \
      org.opencontainers.image.created="${BUILD_TIME}" \
      dev.graphersal.binary.version="${GRAPHERSAL_VERSION}" \
      dev.graphersal.binary.source="${GRAPHERSAL_SOURCE}"

# Layers from rarely to often changed: binary, configuration, store (a knowledge base update
# ships only the last one). Everything belongs to root and is read-only for the server (uid
# 65532); it writes nothing anyway. WORKDIR creates /kb (0755) before the files land in it.
WORKDIR /kb
COPY --chmod=0555 bin/linux-${TARGETARCH}/graphersal /usr/local/bin/graphersal
COPY --chmod=0444 kb/ /kb/
COPY --chmod=0444 store/graph.gstore /kb/graph.gstore

USER 65532:65532
EXPOSE 8080
# SIGTERM drains: /readyz turns 503, running queries get 20 s to finish (docker stop -t 30).
STOPSIGNAL SIGTERM
# Ready once the store and its embedding model are loaded (a few seconds for a 565 MiB store).
# For a much larger store: docker run --health-start-period 5m.
HEALTHCHECK --interval=10s --timeout=3s --start-period=60s --retries=3 \
    CMD ["/usr/local/bin/graphersal", "health", "--ready"]
ENTRYPOINT ["/usr/local/bin/graphersal"]
# The read-only serving mode with read-only MCP and the operator's configuration. To change a
# flag, repeat this whole command after the image name in `docker run`.
CMD ["--graph", "/kb/graph.gstore", "--read-only", "--server", "--host", "0.0.0.0", "--port", "8080", "--mcp", "--mcp-read-only", "--mcp-config", "/kb/mcp-config.json"]
