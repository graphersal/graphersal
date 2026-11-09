# graphersal-serve

**Internal crate, EXPERIMENTAL, no stability guarantees.** The minimal hosting core of Graphersal's
servers for one graph: the session thread and its job queue, the job workers of the scheduled jobs,
the run registry (client-scoped cancel), the playground HTTP API as an HTTP-library-free boundary
(own request and response types), the `Host`/`Origin` checks, the playground UI embedded at build
time (feature `ui-assets`) and the glue that hosts the `graphersal-mcp` endpoint (feature `mcp`).

It depends on no HTTP library: the host converts the requests of its own server to `Request`,
calls `handle` and writes the `Response` back. Today's host is `graphersal --server` (the dev
server of `graphersal-cli`, over `tiny_http`).

It is published because `graphersal-cli` depends on it, and for hosts of Graphersal's own servers.
Its API changes with the servers in any release (a patch release included): a crate that depends on
it pins the exact version (`graphersal-serve = "=0.x.y"`) during 0.x. Applications that query graphs
use the `graphersal` crate instead.

License: Apache-2.0 (see `LICENSE-APACHE` and `NOTICE`).
