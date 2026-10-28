# Security policy

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x | yes (the latest 0.1 release) |

Before 1.0 only the latest minor version receives fixes.

## Reporting a vulnerability

Please report vulnerabilities **privately**, in one of two ways:

- through GitHub's private vulnerability reporting: open the repository's **Security** tab and
  choose **Report a vulnerability**;
- by e-mail to **security@graphersal.dev**.

Do not open a public issue, discussion or pull request for a suspected vulnerability.

Include the affected version (crate, Python package, CLI or playground), a minimal script, program
or file that reproduces the problem, and the impact you expect. You will get an acknowledgement
within a week and an assessment after that. Fixes are released as patch versions and announced
in a GitHub security advisory, which credits you unless you ask otherwise. Please give us a
reasonable time to release a fix before you disclose the issue publicly.

## Scope

In scope, among others:

- **Untrusted queries.** A host may run Gremlin scripts it did not write under `ScriptLimits`, the
  traversal limits, a timeout and the memory budget (book: *Resource Limits*). Exhausting CPU or
  memory despite the documented limits, a panic, a hang or a stack overflow reachable from a
  script is a vulnerability.
- **Permissions.** A script that gets around an `Authorizer` (for example
  `AccessPolicy::read_only()`), a saved query that writes despite being read-only, or a file read or
  written outside a `with_file_root` directory.
- **Untrusted files.** Every decoder treats its input as hostile: GraphSON, GraphML, schemas,
  change sets, snapshots (`.gsnap`), the Store's files and backups. A crash, an unbounded
  allocation or silently wrong data from a crafted file is in scope; damage that is detected and
  reported as damage is the intended behaviour.
- **The dev server** (`graphersal --server`) listens on the loopback interface (inside a container
  on every interface, or where `--host` says) and refuses requests whose `Host` or `Origin` is not
  its own (on a network address it accepts IP literals, `localhost`, dotless names and the
  `--host` name and refuses other host names, against DNS rebinding). A way for a web page or another host to reach
  it, to bypass `--mcp-read-only` or `--mcp-token`, or to make it reveal the AI Chat API key is in
  scope.
- **The playground** must not send a user's graph or queries anywhere (apart from the AI Chat of
  the dev server, which goes to the provider the user configured).

Out of scope:

- the Rust API itself: a host calling it is trusted by design (no limits or permissions unless the
  host installs them);
- the command line tool run on your own scripts: it is a trusted local tool and runs without script
  limits unless asked;
- the dev server's lack of authentication on the loopback interface, which is documented: it is a
  development tool for one user on one machine;
- what an LLM provider does with the data the AI Chat sends to it.
