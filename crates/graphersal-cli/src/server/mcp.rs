//! `graphersal --server --mcp`: the start banner's MCP rows. The endpoint is the crate
//! `graphersal_mcp`, hosted by `graphersal_serve` (feature `mcp`: the `Host`/`Executor` glue
//! over the runtime and `POST /mcp`); the CLI keeps only its flags and banner.

#[cfg(feature = "ai")]
pub(super) use graphersal_serve::current_client;

pub(super) use graphersal_mcp::Mcp;

/// The start banner's MCP rows: where the endpoint is, and the command that connects Claude
/// Code to it.
pub(super) fn banner(mcp: &Mcp, host: &str, port: u16) -> (String, String) {
    let url = format!("http://{host}:{port}/mcp");
    let header = if mcp.has_token() {
        " --header \"Authorization: Bearer <your --mcp-token>\""
    } else {
        ""
    };
    (
        format!(
            "{url}{}{}",
            if mcp.read_only() { " (read-only)" } else { "" },
            if mcp.query_tools() {
                " (saved queries as tools)"
            } else {
                ""
            }
        ),
        format!("claude mcp add --transport http graphersal {url}{header}"),
    )
}
