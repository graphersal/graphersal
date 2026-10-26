# Version Control and Contributions

The short human guide for contributors is `CONTRIBUTING.md` in the repository root; this page summarizes how
changes reach the repository.

## Branches and pull requests

- `main` is the only long-lived branch; every change comes as a pull request against it.
- A pull request keeps the CI workflow green (format, clippy, the test suites, the feature and wasm32 matrices);
  CI runs on a manual start, so run the checks locally (`CONTRIBUTING.md`) and a maintainer starts CI for the
  pull request. When it adds or fixes a Gremlin step, the TinkerPop baseline is updated in the same pull request
  ([TinkerPop Compliance](tinkerpop_compliance.md)).
- A user-visible change is described in the upcoming version's release notes (`book/src/release_notes/`).
- Releases are tags on `main` ([Releases and Versioning](release_plan.md)).

## Commit messages

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/), in English:
`type(scope): summary`, for example `fix(step): has_id() with a numeric predicate` or
`docs(book): the Store's backup page`. Common types are `feat`, `fix`, `docs`, `test`, `refactor`, `perf`,
`build` and `chore`.

## Contributor License Agreement

Graphersal is licensed under Apache-2.0, and contributions are accepted under the Contributor License Agreement
(the `CLA.md` file in the repository root): the author keeps the copyright and grants the maintainer a licence to
use, distribute and sublicense the contribution, including under licence terms other than Apache-2.0, plus a
patent licence. A version already published under Apache-2.0 stays available under it.

The agreement is signed once, by a comment on the first pull request; a CI check (`.github/workflows/cla.yml`)
asks for it and keeps the signatures on the branch `cla-signatures`:

```text
I have read the CLA Document and I hereby sign the CLA
```

A pull request is merged only when every author of its commits has signed. No `Signed-off-by:` line is needed.
