//! The `GType` type tokens (`GType.STRING`/`GType::STRING`).

use crate::script::*;

/// Scope marker that makes the vanilla dotted spelling `GType.STRING` resolve.
///
/// The tokens are registered twice on purpose: as properties of this marker, so a Gremlin query
/// transfers verbatim, and as a static module, so the Rhai path spelling `GType::STRING` works
/// too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptGType;

#[export_module]
#[allow(non_snake_case, non_upper_case_globals)]
pub(crate) mod GTypeModule {
    use super::GType;
    use crate::value::literal::TokenLiteral;

    pub const NULL: GType = GType::NULL;
    pub const BOOLEAN: GType = GType::BOOLEAN;
    pub const INT: GType = GType::INT;
    pub const LONG: GType = GType::LONG;
    pub const FLOAT: GType = GType::FLOAT;
    pub const DOUBLE: GType = GType::DOUBLE;
    pub const NUMBER: GType = GType::NUMBER;
    pub const STRING: GType = GType::STRING;
    pub const UUID: GType = GType::UUID;
    pub const DATETIME: GType = GType::DATETIME;
    pub const DURATION: GType = GType::DURATION;
    pub const DATE: GType = GType::DATE;
    pub const LOCALDATETIME: GType = GType::LOCALDATETIME;
    pub const TIME: GType = GType::TIME;
    pub const OFFSETTIME: GType = GType::OFFSETTIME;
    pub const VECTOR: GType = GType::VECTOR;
    pub const LIST: GType = GType::LIST;
    pub const MAP: GType = GType::MAP;
    pub const VERTEX: GType = GType::VERTEX;
    pub const EDGE: GType = GType::EDGE;
    pub const PATH: GType = GType::PATH;
    pub const GRAPH: GType = GType::GRAPH;

    /// Printing
    #[rhai_fn(global, name = "to_string", name = "to_debug", pure)]
    pub fn to_string(target: &mut GType) -> String {
        target.token_literal()
    }
}
