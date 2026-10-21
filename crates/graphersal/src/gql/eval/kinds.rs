//! Static kinds (stage 3f, user decision U9): a conservative check, run when an expression is
//! compiled, that refuses an operand whose kind is certain from the query text and that the
//! operator or function can never take (`false AND 123`, `1 IN true`, `NOT 'x'`, `type(n)` of a
//! node variable, `size(p)` of a path, `p.name` on a path, `WHERE (n)`, `properties(1)`,
//! `WITH 123 AS x RETURN x.num`). Neo4j refuses these at compile time, and so does the
//! openCypher TCK; the evaluator would raise the same 22G03 only when a row reaches the
//! expression (never on an empty graph, nor behind a short-circuit `AND` / `OR`).
//!
//! A [`Kind`] is the set of value kinds an expression may give (`NULL`, boolean, integer, float,
//! string, list, map, node, edge, path, any other scalar), with the item kinds of a list one
//! level deep. Kinds come only from what is certain:
//!
//! - literals, and lists and maps built from them (a `$parameter` is any kind: its value is
//!   never looked at, so a query is refused or accepted whatever the parameters are);
//! - the variables of a pattern (node, edge, path; a `WITH` column holding one), through
//!   [`Scope::kind`] (any other name, a group variable and a `FOR` item are any kind);
//! - a `LET` / projecting-`WITH` value, which the binder substitutes by its expression;
//! - the comprehension variable (the item kind of its list), the result kinds of operators
//!   and of the functions listed in [`Walk::call`]; a subscript `x[i]` of a list of kind `k`
//!   is `k` (or `NULL`), a slice the list (a subscript's target and index, and the list of a
//!   comprehension, keep their run-time checks, whose messages name the form).
//!
//! An operand is refused only when every kind it may give, `NULL` left aside, is one the place
//! cannot take; a `NULL` literal never is (`NULL AND TRUE` is fine). A `NULL` operand of an
//! arithmetic operator makes it `NULL` before the other operand is looked at (`'a' - NULL`),
//! as the evaluator does. The error is 22G03 with the advice of
//! [`GqlHelpTopic::StaticKind`]; everything else keeps its run-time check.
//!
//! Left spines (`a OR b OR ...`, `n.a.b`) are loops, as in [`super::compile`], so the walk
//! recurses at most about `parser::MAX_DEPTH` levels.

use super::super::ast::{
    BinaryOp, CallArgs, CaseExpr, CastTarget, ComprehensionForm, Expr, ExprKind, ListComprehension,
    Literal, MapProjection, ProjectionItem, SubqueryKind, Subscript, UnaryOp,
};
use super::super::lexer::Span;
use super::super::parser::{Function, lookup_function};
use super::{EvalError, Scope, chain_left, function_name};
use crate::error::GqlHelpTopic;

/// The value kinds an expression may give (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Kind {
    /// The kinds of the value (the `K_*` bits).
    bits: u16,
    /// When the value may be a list: the kinds of its items (a nested list's items are any).
    items: u16,
}

const K_NULL: u16 = 1;
const K_BOOL: u16 = 1 << 1;
const K_INT: u16 = 1 << 2;
const K_FLOAT: u16 = 1 << 3;
const K_STRING: u16 = 1 << 4;
const K_LIST: u16 = 1 << 5;
const K_MAP: u16 = 1 << 6;
const K_NODE: u16 = 1 << 7;
const K_EDGE: u16 = 1 << 8;
const K_PATH: u16 = 1 << 9;
/// Any other scalar (a uuid, a vector, ...).
const K_OTHER: u16 = 1 << 10;
/// A temporal value (stage 3g): it has components (`d.year`) and takes `+` / `-` / `*` / `/`
/// with a duration or a number, so those places accept it (no false static errors).
const K_TEMPORAL: u16 = 1 << 11;
const K_ALL: u16 = K_NULL
    | K_BOOL
    | K_INT
    | K_FLOAT
    | K_STRING
    | K_LIST
    | K_MAP
    | K_NODE
    | K_EDGE
    | K_PATH
    | K_OTHER
    | K_TEMPORAL;
const K_NUMBER: u16 = K_INT | K_FLOAT;
const K_ELEMENT: u16 = K_NODE | K_EDGE;

/// The names of the kinds, in bit order (for messages).
const NAMES: [&str; 12] = [
    "NULL",
    "a boolean",
    "an integer",
    "a float",
    "a string",
    "a list",
    "a map",
    "a node",
    "an edge",
    "a path",
    "a value",
    "a temporal value",
];

impl Kind {
    /// Any value.
    pub(crate) const ANY: Kind = Kind {
        bits: K_ALL,
        items: K_ALL,
    };
    /// A node (or `NULL`).
    pub(crate) const NODE: Kind = Kind::of(K_NODE | K_NULL);
    /// An edge (or `NULL`).
    pub(crate) const EDGE: Kind = Kind::of(K_EDGE | K_NULL);
    /// A path (or `NULL`).
    pub(crate) const PATH: Kind = Kind::of(K_PATH | K_NULL);

    const fn of(bits: u16) -> Kind {
        Kind {
            bits,
            items: if bits & K_LIST != 0 { K_ALL } else { 0 },
        }
    }

    const fn list(items: u16) -> Kind {
        Kind {
            bits: K_LIST,
            items,
        }
    }

    fn or_null(self) -> Kind {
        Kind {
            bits: self.bits | K_NULL,
            items: self.items,
        }
    }

    fn join(self, other: Kind) -> Kind {
        Kind {
            bits: self.bits | other.bits,
            items: self.items | other.items,
        }
    }

    /// The kinds besides `NULL`.
    fn certain(self) -> u16 {
        self.bits & !K_NULL
    }

    /// Whether it is certainly `NULL`.
    fn null_only(self) -> bool {
        self.bits == K_NULL
    }

    /// The kind of an item, when the value is certainly a list (else any).
    fn item(self) -> Kind {
        if self.certain() == K_LIST {
            Kind::of(self.items)
        } else {
            Kind::ANY
        }
    }

    /// The kind as a message names it ("an integer", "a string or a list").
    fn describe(self) -> String {
        let names: Vec<&str> = NAMES
            .iter()
            .enumerate()
            .filter(|(bit, _)| self.certain() & (1 << bit) != 0)
            .map(|(_, name)| *name)
            .collect();
        names.join(" or ")
    }
}

/// Checks the static kinds of `expr` (the module docs) against `scope`.
pub(crate) fn check(expr: &Expr, scope: &dyn Scope) -> Result<(), EvalError> {
    Walk {
        scope,
        locals: Vec::new(),
    }
    .kind(expr)
    .map(|_| ())
}

/// Checks `expr` as a condition: its static kinds, and that it can be a boolean (`WHERE (n)`,
/// `WHERE 1` are refused).
pub(crate) fn check_condition(expr: &Expr, scope: &dyn Scope) -> Result<(), EvalError> {
    let mut walk = Walk {
        scope,
        locals: Vec::new(),
    };
    walk.condition(expr)
}

/// One walk over an expression: the scope and the comprehension variables around the part
/// being walked, innermost last.
struct Walk<'s, 'e> {
    scope: &'s dyn Scope,
    locals: Vec<(&'e str, Kind)>,
}

/// The error of an operand of a refused kind.
fn refused(message: String, span: Span) -> EvalError {
    EvalError::invalid_type(message, span).with_topic(GqlHelpTopic::StaticKind)
}

impl<'e> Walk<'_, 'e> {
    /// Refuses `kind` (of the operand at `span`) when none of its kinds is in `accept`.
    fn require(
        &self,
        kind: Kind,
        accept: u16,
        span: Span,
        message: impl FnOnce(String) -> String,
    ) -> Result<(), EvalError> {
        if kind.certain() != 0 && kind.certain() & accept == 0 {
            return Err(refused(message(kind.describe()), span));
        }
        Ok(())
    }

    fn condition(&mut self, expr: &'e Expr) -> Result<(), EvalError> {
        let kind = self.kind(expr)?;
        self.require(kind, K_BOOL, expr.span, |k| {
            format!("a condition must be a boolean, not {k}: it can never be TRUE")
        })
    }

    // A thin dispatch: left spines are loops (`chain`), rare forms live in functions of their
    // own, so a level of a tall expression costs a small frame.
    fn kind(&mut self, expr: &'e Expr) -> Result<Kind, EvalError> {
        if chain_left(expr).is_some() {
            return self.chain(expr);
        }
        Ok(match &expr.kind {
            ExprKind::Literal(literal) => Kind::of(match literal {
                Literal::Null => K_NULL,
                Literal::Bool(_) => K_BOOL,
                Literal::Integer(_) => K_INT,
                Literal::Float(_) => K_FLOAT,
                Literal::String(_) => K_STRING,
            }),
            ExprKind::Parameter(_) => Kind::ANY,
            ExprKind::Variable(var) => self.variable(&var.name),
            _ => self.other(expr)?,
        })
    }

    fn variable(&self, name: &str) -> Kind {
        if let Some((_, kind)) = self.locals.iter().rev().find(|(local, _)| *local == name) {
            return *kind;
        }
        // A comprehension variable of an enclosing compilation: its kind is not known here.
        if self.scope.local(name).is_some() {
            return Kind::ANY;
        }
        self.scope.kind(name)
    }

    #[inline(never)]
    fn other(&mut self, expr: &'e Expr) -> Result<Kind, EvalError> {
        Ok(match &expr.kind {
            ExprKind::List(items) => {
                let mut joined = 0;
                for item in items {
                    joined |= self.kind(item)?.bits;
                }
                Kind::list(joined)
            }
            ExprKind::Map(map) => {
                for entry in &map.entries {
                    self.kind(&entry.value)?;
                }
                Kind::of(K_MAP)
            }
            ExprKind::Unary { op, operand } => self.unary(*op, operand)?,
            ExprKind::Case(case) => self.case(case)?,
            ExprKind::FunctionCall { name, args, .. } => self.call(&name.name, args)?,
            // The body is compiled on its own (its own scope).
            ExprKind::Subquery(sub) => match sub.kind {
                SubqueryKind::Exists => Kind::of(K_BOOL),
                SubqueryKind::Count => Kind::of(K_INT),
                SubqueryKind::Collect => Kind::of(K_LIST),
            },
            ExprKind::PatternComprehension(_) => Kind::of(K_LIST),
            ExprKind::MapProjection(projection) => self.projection(projection)?,
            ExprKind::ListComprehension(comprehension) => self.comprehension(comprehension)?,
            // `kind` handles these.
            ExprKind::Literal(_)
            | ExprKind::Parameter(_)
            | ExprKind::Variable(_)
            | ExprKind::Binary { .. }
            | ExprKind::Property { .. }
            | ExprKind::IsNull { .. }
            | ExprKind::IsLabeled { .. }
            | ExprKind::IsEndpoint { .. }
            | ExprKind::Index(_) => Kind::ANY,
        })
    }

    /// A left spine bottom-up, as `compile_chain` builds it.
    #[inline(never)]
    fn chain(&mut self, expr: &'e Expr) -> Result<Kind, EvalError> {
        let mut spine = Vec::new();
        let mut bottom = expr;
        while let Some(left) = chain_left(bottom) {
            spine.push(bottom);
            bottom = left;
        }
        let mut kind = self.kind(bottom)?;
        let mut left_span = bottom.span;
        for link in spine.iter().rev() {
            kind = self.link(link, kind, left_span)?;
            left_span = link.span;
        }
        Ok(kind)
    }

    /// The kind of one chain link over a left operand of kind `left` written at `at`.
    fn link(&mut self, link: &'e Expr, left: Kind, at: Span) -> Result<Kind, EvalError> {
        Ok(match &link.kind {
            ExprKind::Binary { op, right, .. } => {
                let right_kind = self.kind(right)?;
                self.binary(*op, (left, at), (right_kind, right.span))?
            }
            ExprKind::Property { key, .. } => {
                self.require(left, K_ELEMENT | K_MAP | K_TEMPORAL, at, |k| {
                    format!(
                        "cannot read the property `{}` of {k}: only a node, an edge or a map \
                         has properties",
                        key.name
                    )
                })?;
                Kind::ANY
            }
            ExprKind::IsNull { .. } => Kind::of(K_BOOL),
            ExprKind::IsLabeled { .. } => {
                self.require(left, K_ELEMENT, at, |k| {
                    format!("a label test takes a node or an edge, not {k}")
                })?;
                Kind::of(K_BOOL | K_NULL)
            }
            ExprKind::IsEndpoint { edge, .. } => {
                self.kind(edge)?;
                Kind::of(K_BOOL | K_NULL)
            }
            // The target and index kinds of a subscript keep their run-time check (with
            // `GqlHelpTopic::Subscript`); only the result kind is used here.
            ExprKind::Index(index) => match &index.subscript {
                Subscript::At(at_index) => {
                    self.kind(at_index)?;
                    left.item().or_null()
                }
                Subscript::Slice { from, to } => {
                    for bound in [from, to].into_iter().flatten() {
                        self.kind(bound)?;
                    }
                    if left.certain() == K_LIST {
                        Kind::list(left.items).or_null()
                    } else {
                        Kind::ANY
                    }
                }
            },
            // `chain_left` admits only the kinds above.
            _ => Kind::ANY,
        })
    }

    fn binary(
        &self,
        op: BinaryOp,
        (left, left_at): (Kind, Span),
        (right, right_at): (Kind, Span),
    ) -> Result<Kind, EvalError> {
        Ok(match op {
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                let name = match op {
                    BinaryOp::And => "AND",
                    BinaryOp::Or => "OR",
                    _ => "XOR",
                };
                for (kind, at) in [(left, left_at), (right, right_at)] {
                    self.require(kind, K_BOOL, at, |k| {
                        format!("{name} takes booleans, not {k}")
                    })?;
                }
                Kind::of(K_BOOL | K_NULL)
            }
            BinaryOp::In => {
                self.require(right, K_LIST, right_at, |k| {
                    format!("IN tests a value against a list, not {k}")
                })?;
                Kind::of(K_BOOL | K_NULL)
            }
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
            | BinaryOp::StartsWith
            | BinaryOp::EndsWith
            | BinaryOp::Contains => Kind::of(K_BOOL | K_NULL),
            BinaryOp::Concat => Kind::ANY,
            BinaryOp::Add => {
                if (left.bits | right.bits) & !(K_NUMBER | K_NULL) == 0 {
                    numeric(left, right)
                } else {
                    Kind::ANY
                }
            }
            BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod | BinaryOp::Pow => {
                // A NULL operand gives NULL before the other one is looked at.
                if !left.null_only() && !right.null_only() {
                    // Stage 3g: `x - dur`, `dur * n`, `dur / n` take temporal operands.
                    let accept = match op {
                        BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => K_NUMBER | K_TEMPORAL,
                        _ => K_NUMBER,
                    };
                    for (kind, at) in [(left, left_at), (right, right_at)] {
                        self.require(kind, accept, at, |k| {
                            format!("arithmetic needs numbers, not {k}")
                        })?;
                    }
                }
                if op == BinaryOp::Pow {
                    Kind::of(K_FLOAT | K_NULL)
                } else {
                    numeric(left, right)
                }
            }
        })
    }

    #[inline(never)]
    fn unary(&mut self, op: UnaryOp, operand: &'e Expr) -> Result<Kind, EvalError> {
        match op {
            UnaryOp::Not => {
                let kind = self.kind(operand)?;
                self.require(kind, K_BOOL, operand.span, |k| {
                    format!("NOT takes a boolean, not {k}")
                })?;
                Ok(Kind::of(K_BOOL | K_NULL))
            }
            UnaryOp::Minus | UnaryOp::Plus => {
                let kind = self.kind(operand)?;
                self.require(kind, K_NUMBER | K_TEMPORAL, operand.span, |k| {
                    format!("a sign applies to a number, not {k}")
                })?;
                Ok(Kind::of(
                    (kind.bits & (K_NUMBER | K_TEMPORAL | K_NULL)) | K_NULL,
                ))
            }
        }
    }

    #[inline(never)]
    fn case(&mut self, case: &'e CaseExpr) -> Result<Kind, EvalError> {
        if let Some(operand) = &case.operand {
            self.kind(operand)?;
        }
        let mut result = Kind { bits: 0, items: 0 };
        for arm in &case.whens {
            if case.operand.is_some() {
                self.kind(&arm.when)?;
            } else {
                self.condition(&arm.when)?;
            }
            result = result.join(self.kind(&arm.then)?);
        }
        Ok(match &case.else_result {
            Some(else_result) => result.join(self.kind(else_result)?),
            None => result.or_null(),
        })
    }

    #[inline(never)]
    fn projection(&mut self, projection: &'e MapProjection) -> Result<Kind, EvalError> {
        let target = self.kind(&projection.target)?;
        self.require(target, K_ELEMENT | K_MAP, projection.target.span, |k| {
            format!("a map projection reads a node, an edge or a map, not {k}")
        })?;
        for item in &projection.items {
            if let ProjectionItem::Entry(entry) = item {
                self.kind(&entry.value)?;
            }
        }
        Ok(Kind::of(K_MAP | K_NULL))
    }

    #[inline(never)]
    fn comprehension(&mut self, comprehension: &'e ListComprehension) -> Result<Kind, EvalError> {
        // A list of another kind keeps its run-time check (its message names the form).
        let list = self.kind(&comprehension.list)?;
        let pushed = self.locals.len();
        if let ComprehensionForm::Reduce(accumulator) = &comprehension.form {
            self.kind(&accumulator.init)?;
            self.locals
                .push((accumulator.variable.name.as_str(), Kind::ANY));
        }
        // An empty list binds no item: its parts never run.
        let item = if list.certain() == K_LIST && list.items != 0 {
            list.item()
        } else {
            Kind::ANY
        };
        self.locals
            .push((comprehension.variable.name.as_str(), item));
        let parts = (|| {
            if let Some(filter) = &comprehension.filter {
                self.condition(filter)?;
            }
            comprehension
                .map
                .as_ref()
                .map(|map| self.kind(map))
                .transpose()
        })();
        self.locals.truncate(pushed);
        let mapped = parts?;
        Ok(match &comprehension.form {
            ComprehensionForm::List => {
                Kind::list(mapped.map_or(item.bits, |kind| kind.bits)).or_null()
            }
            ComprehensionForm::Predicate(_) => Kind::of(K_BOOL | K_NULL),
            ComprehensionForm::Reduce(_) => Kind::ANY,
        })
    }

    /// A function call: its arguments' kinds, the checks of the functions that take one kind
    /// of value, and the result kinds this module knows (any other function gives any kind).
    #[inline(never)]
    fn call(&mut self, name: &str, args: &'e CallArgs) -> Result<Kind, EvalError> {
        let exprs = match args {
            CallArgs::Star(_) => return Ok(Kind::of(K_INT)),
            CallArgs::Cast {
                operand, target, ..
            } => {
                self.kind(operand)?;
                return Ok(Kind::of(
                    K_NULL
                        | match target {
                            CastTarget::Integer => K_INT,
                            CastTarget::Float => K_FLOAT,
                            CastTarget::String => K_STRING,
                            CastTarget::Boolean => K_BOOL,
                        },
                ));
            }
            CallArgs::Trim {
                character, source, ..
            } => {
                if let Some(character) = character {
                    self.kind(character)?;
                }
                self.kind(source)?;
                return Ok(Kind::of(K_STRING | K_NULL));
            }
            CallArgs::Exprs(exprs) => exprs,
        };
        let mut first = None;
        for (i, arg) in exprs.iter().enumerate() {
            let kind = self.kind(arg)?;
            if i == 0 {
                first = Some((kind, arg.span));
            }
        }
        let Some(spec) = lookup_function(name) else {
            return Ok(Kind::ANY);
        };
        // Stage 3g: a temporal function gives a temporal value whatever its arguments (the
        // clocks take none).
        if let Function::Temporal(_) = spec.function {
            return Ok(Kind::of(K_TEMPORAL | K_NULL));
        }
        let Some((arg, at)) = first else {
            return Ok(Kind::ANY);
        };
        let function = spec.function;
        let takes = |accept: u16, what: &str| {
            self.require(arg, accept, at, |k| {
                format!("{}() takes {what}, not {k}", function_name(function))
            })
        };
        Ok(match function {
            Function::ElementId => {
                takes(K_ELEMENT, "a node or an edge")?;
                Kind::of(K_STRING | K_NULL)
            }
            Function::Labels => {
                takes(K_ELEMENT, "a node or an edge")?;
                Kind::list(K_STRING).or_null()
            }
            Function::Type => {
                if arg.certain() == K_NODE {
                    return Err(refused(
                        "type() takes an edge, not a node; use labels(v) for a node's labels"
                            .to_string(),
                        at,
                    ));
                }
                takes(K_EDGE, "an edge")?;
                Kind::of(K_STRING | K_NULL)
            }
            Function::Upper | Function::Lower => {
                takes(K_STRING, "a string")?;
                Kind::of(K_STRING | K_NULL)
            }
            Function::CharLength => {
                takes(K_STRING, "a string")?;
                Kind::of(K_INT | K_NULL)
            }
            Function::Size | Function::Cardinality => {
                takes(K_STRING | K_LIST, "a string or a list")?;
                Kind::of(K_INT | K_NULL)
            }
            Function::PathLength => {
                takes(K_PATH, "a path")?;
                Kind::of(K_INT | K_NULL)
            }
            Function::Keys => {
                takes(K_ELEMENT | K_MAP, "a node, an edge or a map")?;
                Kind::list(K_STRING).or_null()
            }
            Function::Properties => {
                takes(K_ELEMENT | K_MAP, "a node, an edge or a map")?;
                Kind::of(K_MAP | K_NULL)
            }
            Function::Count => Kind::of(K_INT),
            Function::CollectList => Kind::list(arg.certain()),
            Function::StartNode | Function::EndNode => Kind::NODE,
            Function::Nodes => Kind::list(K_NODE).or_null(),
            Function::Relationships => Kind::list(K_EDGE).or_null(),
            Function::Head | Function::Last => arg.item().or_null(),
            Function::Tail => {
                if arg.certain() == K_LIST {
                    Kind::list(arg.items).or_null()
                } else {
                    Kind::ANY
                }
            }
            Function::Range => Kind::list(K_INT).or_null(),
            Function::Split => Kind::list(K_STRING).or_null(),
            Function::ToInteger | Function::ToIntegerOrNull => Kind::of(K_INT | K_NULL),
            Function::ToFloat | Function::ToFloatOrNull => Kind::of(K_FLOAT | K_NULL),
            Function::ToBoolean | Function::ToBooleanOrNull => Kind::of(K_BOOL | K_NULL),
            Function::ToString | Function::ToStringOrNull => Kind::of(K_STRING | K_NULL),
            Function::ByteLength => Kind::of(K_INT | K_NULL),
            Function::IsEmpty => Kind::of(K_BOOL | K_NULL),
            // Taken above: every temporal function gives a temporal value or `NULL`
            // (`duration.inMonths` and its siblings give durations, as in Neo4j).
            Function::Temporal(_) => Kind::of(K_TEMPORAL | K_NULL),
            Function::Coalesce
            | Function::Sum
            | Function::Avg
            | Function::Min
            | Function::Max
            | Function::PercentileCont
            | Function::PercentileDisc
            | Function::Cast
            | Function::Left
            | Function::Right
            | Function::Substring
            | Function::Trim
            | Function::Btrim
            | Function::Ltrim
            | Function::Rtrim
            | Function::Replace
            | Function::Reverse
            | Function::NullIf
            | Function::Abs
            | Function::Floor
            | Function::Ceil
            | Function::Round
            | Function::Sign
            | Function::Sqrt
            | Function::Exp
            | Function::Ln
            | Function::Log
            | Function::Log10
            | Function::Power
            | Function::Mod => Kind::ANY,
        })
    }
}

/// The kind of an arithmetic result of two operands.
fn numeric(left: Kind, right: Kind) -> Kind {
    let both = left.bits | right.bits;
    if both & !(K_INT | K_NULL) == 0 {
        Kind::of(K_INT | K_NULL)
    } else if both & !(K_NUMBER | K_NULL) == 0 {
        Kind::of(K_NUMBER | K_NULL)
    } else {
        Kind::ANY
    }
}
