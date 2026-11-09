# graphersal-mcp

**Internal crate, EXPERIMENTAL, no stability guarantees.** The Model Context Protocol endpoint of
Graphersal's servers: the JSON-RPC protocol (MCP Streamable HTTP with plain JSON answers), the tool
catalog and its handlers over a `graphersal-session` session, the resources, and the shaping of
answers (compact JSON, paging, the answer limit).

It depends on no HTTP library, no async runtime and no particular way of running jobs: the host
server adapts its HTTP requests to `McpRequest`/`McpResponse`, or serves a `Connection` (one MCP
session) over a byte stream with MCP's stdio framing (`serve_lines`), and runs the session jobs
through its own `Executor`.
Today's host is `graphersal --server --mcp` (the dev server of `graphersal-cli`), whose AI Chat
uses the same tools.

It is published because `graphersal-cli` depends on it, and for hosts of Graphersal's own servers.
Its API changes with the servers in any release (a patch release included): a crate that depends on
it pins the exact version (`graphersal-mcp = "=0.x.y"`) during 0.x. Applications that query graphs
use the `graphersal` crate instead.

License: Apache-2.0 (see `LICENSE-APACHE` and `NOTICE`).
