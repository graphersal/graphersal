//! Skipping tests that depend on the machine's CPU or memory on CI runners.

/// Whether this run is on a CI runner (`CI` set and not empty, "false" or "0", as GitHub Actions
/// sets it). A test whose result depends on how much CPU time or memory the machine gives it
/// (an upper bound on wall-clock time, a speed ratio, measured process memory) returns early
/// there, because a shared runner guarantees neither; such tests run locally, before every
/// release in particular. Prints why it skipped.
pub fn skip_on_ci(what: &str) -> bool {
    let on_ci = std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "false" && v != "0");
    if on_ci {
        eprintln!("skipped on CI ({what}): depends on the runner's CPU or memory");
    }
    on_ci
}
