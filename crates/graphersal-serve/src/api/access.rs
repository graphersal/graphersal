//! The per-request access level (EXPERIMENTAL; plan `ai/graphersal-server-access/PLAN.md` A3):
//! what a request the host marked read-only ([`crate::Request::mark_read_only`]) may do. ONE
//! explicit table classifies every route of the core ([`route_access`]); a test walks the route
//! arms of `api/mod.rs`, `api/store.rs` and `api/layout.rs` so a new route cannot stay
//! unclassified.
//!
//! The runtime read-only toggle's commit hook is not reused per request: it is graph-wide, and
//! registering it turns on change capture for every commit.

use crate::http::Method;

/// How a route of the core treats a request marked read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteAccess {
    /// Reads (or computes without writing): answered as on any request.
    Read,
    /// Runs a script or a saved query: under the reading authorizer on a marked request (a
    /// write fails with `PermissionDenied` and changes nothing).
    Query,
    /// Writes (a commit, a Store operation, a file, the shared session's view, the workload
    /// sidecar): `409` on a marked request.
    Write,
}

/// The access class of `method` on the API endpoint `endpoint` (the path without `/api/`), or
/// `None` when the core has no such route (a wrong method is answered `405`, an unknown endpoint
/// goes to the host's extra routes, which read the mark themselves).
pub(crate) fn route_access(method: &Method, endpoint: &str) -> Option<RouteAccess> {
    use RouteAccess::{Query, Read, Write};
    let access = match (method, endpoint) {
        (Method::Post, "execute" | "query") => Query,
        (
            Method::Get,
            "info"
            | "changes"
            | "mcp/stats"
            | "admin/runs"
            | "graph"
            | "schema"
            | "schema/stored"
            | "schema/infer"
            | "graph-view"
            | "meta-graph"
            | "element"
            | "element/edit"
            | "completions"
            | "token-values"
            | "memory"
            | "jobs"
            | "jobs/history"
            | "workload"
            | "journal"
            | "statistics"
            | "export"
            | "queries"
            | "queries/get"
            | "catalog/export"
            | "compression"
            | "indexes"
            | "store"
            | "store/summary"
            | "store/details"
            | "store/snapshot"
            | "store/attic/changes"
            | "store/compaction",
        ) => Read,
        (
            Method::Post,
            "cancel"
            | "schema/view"
            | "schema/validate"
            | "schema/validate-patch"
            | "schema/diff"
            | "graph-view"
            | "graph-filter-query"
            | "neighbourhood"
            | "sample"
            | "element/labels"
            | "advise"
            | "memory/report"
            | "layer/diff"
            | "queries/preview"
            | "queries/call-text"
            | "compression/estimate"
            | "layout"
            | "layout/step"
            | "layout/cancel"
            | "store/verify",
        ) => Read,
        (
            Method::Post,
            "admin/runs/cancel"
            | "schema/set"
            | "schema/patch"
            | "element/update"
            | "element/create"
            | "element/delete"
            | "layer/reset"
            | "advise/apply"
            | "advise/dismiss"
            | "advise/restore"
            | "jobs/define"
            | "jobs/drop"
            | "jobs/enable"
            | "jobs/schedule"
            | "jobs/run"
            | "save"
            | "queries/define"
            | "queries/drop"
            | "queries/move"
            | "catalog/load"
            | "compression/define"
            | "compression/drop"
            | "compression/drop-many"
            | "compression/recompress"
            | "indexes/create"
            | "indexes/drop"
            | "store/checkpoint"
            | "store/mark"
            | "store/view"
            | "store/live"
            | "store/fork"
            | "store/rollback"
            | "store/attic/restore"
            | "store/attic/fork"
            | "store/attic/remove"
            | "store/backup"
            | "store/compact",
        ) => Write,
        _ => return None,
    };
    Some(access)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `(Method::Get|Post, "endpoint")` route arm of a router source file.
    fn route_arms(source: &str) -> Vec<(Method, String)> {
        let mut arms = Vec::new();
        for (prefix, method) in [
            ("(Method::Get,", Method::Get),
            ("(Method::Post,", Method::Post),
        ] {
            let mut rest = source;
            while let Some(at) = rest.find(prefix) {
                rest = &rest[at + prefix.len()..];
                let tail = rest.trim_start();
                let Some(tail) = tail.strip_prefix('"') else {
                    continue;
                };
                if let Some(end) = tail.find('"') {
                    arms.push((method.clone(), tail[..end].to_string()));
                }
            }
        }
        arms
    }

    /// Every endpoint named in a router's `405` arm (`(_, "a" | "b" ...)`).
    fn endpoints_of_405_arms(source: &str) -> Vec<String> {
        let mut endpoints = Vec::new();
        let mut rest = source;
        while let Some(at) = rest.find("(\n            _,\n") {
            rest = &rest[at..];
            let end = rest.find(") =>").unwrap_or(rest.len());
            let arm = &rest[..end];
            for piece in arm.split('"').skip(1).step_by(2) {
                endpoints.push(piece.to_string());
            }
            rest = &rest[end..];
        }
        endpoints
    }

    #[test]
    fn every_route_of_the_core_is_classified() {
        let sources = [
            include_str!("mod.rs"),
            include_str!("store.rs"),
            include_str!("admin.rs"),
        ];
        let mut arms: Vec<(Method, String)> = sources.iter().flat_map(|s| route_arms(s)).collect();
        // `api/layout.rs` routes by endpoint name, POST only.
        for endpoint in ["layout", "layout/step", "layout/cancel"] {
            arms.push((Method::Post, endpoint.to_string()));
        }
        assert!(arms.len() > 80, "the route arms were not found: {arms:?}");
        for (method, endpoint) in &arms {
            assert!(
                route_access(method, endpoint).is_some(),
                "{method} /api/{endpoint} has no access class: add it to route_access"
            );
        }
        // Every endpoint a `405` arm knows has at least one classified method.
        let known: Vec<String> = sources[..2]
            .iter()
            .flat_map(|s| endpoints_of_405_arms(s))
            .collect();
        assert!(known.len() > 70, "the 405 arms were not found: {known:?}");
        for endpoint in &known {
            assert!(
                route_access(&Method::Get, endpoint).is_some()
                    || route_access(&Method::Post, endpoint).is_some(),
                "/api/{endpoint} has no access class: add it to route_access"
            );
        }
    }

    #[test]
    fn a_layer_reset_is_a_write() {
        assert_eq!(
            route_access(&Method::Post, "layer/reset"),
            Some(RouteAccess::Write)
        );
        assert_eq!(route_access(&Method::Get, "layer/reset"), None);
    }

    #[test]
    fn the_classes_of_the_routes_a_reader_must_not_use() {
        for endpoint in [
            "store/fork",
            "store/attic/fork",
            "store/backup",
            "store/view",
            "store/live",
            "advise/dismiss",
            "advise/restore",
            "save",
            "jobs/define",
            "schema/set",
        ] {
            assert_eq!(
                route_access(&Method::Post, endpoint),
                Some(RouteAccess::Write),
                "{endpoint}"
            );
        }
        assert_eq!(
            route_access(&Method::Post, "execute"),
            Some(RouteAccess::Query)
        );
        assert_eq!(
            route_access(&Method::Get, "export"),
            Some(RouteAccess::Read)
        );
        assert_eq!(
            route_access(&Method::Get, "store/snapshot"),
            Some(RouteAccess::Read)
        );
        // A wrong method or an unknown endpoint is no route of the core.
        assert_eq!(route_access(&Method::Get, "schema/set"), None);
        assert_eq!(route_access(&Method::Post, "ai/conversations"), None);
    }
}
