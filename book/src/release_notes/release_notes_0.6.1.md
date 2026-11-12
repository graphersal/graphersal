# Release 0.6.1

A patch release of the 0.6 line that fixes a crash of the playground session and the dev server
on scripts holding a multi-byte character. It changes no format and no API; stores, snapshots
and journals of 0.6.0 are read and written exactly as before.

## Fixed

- **A multi-byte character before a script's last call no longer crashes the session.** To show
  the result of the last call, the session looks for the terminal call of a script by searching
  back from its last `(`. It cut the script one byte after the character it stopped at, which
  fell inside that character when it was not ASCII: a script with, for example, Cyrillic text in
  a string literal and an unbalanced parenthesis after it made the session thread panic. The
  playground's engine and the dev server (with its MCP tools) both run this session, so such a
  query failed with an internal error instead of its result. The search now steps over whole
  characters.
- **The REPL's command completion no longer panics on a multi-byte space.** The same pattern in
  the completion of `/` commands cut a line holding, for example, a no-break space (U+00A0)
  inside that character.

## Dependencies

- `rustix` 1.1.5 -> 1.1.6 in the lock file of the binaries (RUSTSEC-2026-0486, unsound
  extended-attribute functions; Graphersal does not call them, it reaches `rustix` through
  `fd-lock`).

## Upgrading from 0.6.0

A drop-in replacement: no format change, no API change, no upgrade step. Stores, snapshots,
journals and backups of 0.6.0 and 0.6.1 are interchangeable.
