//! Display options and bounded rendering of traversal results.
//!
//! Only *display* is bounded, never data: `to_list()`, `next()` and the other data terminals
//! always return everything. [`RenderOptions::max_rows`] and [`RenderOptions::max_items`] limit
//! how much of a result a visualizer turns into text, and the rendering functions report what they
//! cut in a [`Truncation`] next to the text, never inside it.

use std::cell::Cell;

use crate::value::{ElementProperty, PropertyMap};

/// Options for rendering traversal results as text: table layout and display limits.
///
/// Used by every visualizer format (table, markdown, JSON, tree, ...). The table-only fields
/// (`columns`, `max_width`, `truncate`, `row_numbers`) are ignored by the other formats.
///
/// Both display limits default to `None` (unlimited): a library caller gets everything unless it
/// asks for limits. Front ends such as `graphersal` set their own defaults.
///
/// ```rust
/// use graphersal::prelude::*;
/// use graphersal::exec::RenderOptions;
///
/// let graph = TraversalGraph::tinkerpop_modern();
/// let options = RenderOptions::new().with_max_rows(2);
/// let rendered = graph
///     .traversal()
///     .v(None)
///     .visualize_with(VisualizeFormat::Table, &options)
///     .unwrap();
/// let truncation = rendered.truncation.expect("6 vertices, 2 shown");
/// assert_eq!(truncation.rows_shown, 2);
/// assert!(truncation.more_rows);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct RenderOptions {
    /// Explicit subset of columns to display or exclude (tables only).
    /// - Inclusion: `["name", "age"]` displays only `name` and `age` in that order.
    /// - Exclusion: `["!id", "!label"]` (or `-id`, `-label`) displays all discovered columns except `id` and `label`.
    /// - None: all unique element properties are discovered and formatted automatically.
    pub columns: Option<Vec<String>>,
    /// Wrap text inside cells across multiple lines when exceeding this character width.
    pub max_width: Option<usize>,
    /// Truncate text inside cells with "..." when exceeding this character length.
    pub truncate: Option<usize>,
    /// Show a 1-based row number column (`#`) as the first column.
    pub row_numbers: bool,
    /// How many top-level results (rows, traversers, array items) are rendered; `None` renders
    /// all of them.
    pub max_rows: Option<usize>,
    /// How many items of any nested collection (an array element or an object entry inside a
    /// row, at any depth) are rendered; `None` renders all of them.
    pub max_items: Option<usize>,
    /// The spelling of step names in diagnostics (`.profile()`, error messages): a front end
    /// passes it on as the session option `render.spelling` (see [`Spelling`](crate::traversal::Spelling)). Results are
    /// never affected.
    #[cfg_attr(feature = "serde", serde(default))]
    pub spelling: crate::traversal::Spelling,
}

impl RenderOptions {
    /// Sets the spelling of step names in diagnostics.
    pub fn with_spelling(mut self, spelling: crate::traversal::Spelling) -> Self {
        self.spelling = spelling;
        self
    }

    /// Creates default options: automatic columns and no display limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// Options without any display limit, the library default. Same as [`Self::new`], spelled
    /// out for call sites that mean "unlimited".
    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Sets explicit column inclusion or exclusion list.
    pub fn with_columns<I, S>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.columns = Some(columns.into_iter().map(Into::into).collect());
        self
    }

    /// Sets maximum column cell width for line wrapping.
    pub fn with_max_width(mut self, max_width: usize) -> Self {
        self.max_width = Some(max_width);
        self
    }

    /// Sets truncation length with ellipsis.
    pub fn with_truncate(mut self, truncate: usize) -> Self {
        self.truncate = Some(truncate);
        self
    }

    /// Toggles row numbering column (`#`).
    pub fn with_row_numbers(mut self, row_numbers: bool) -> Self {
        self.row_numbers = row_numbers;
        self
    }

    /// Renders at most `max_rows` top-level results. `0` is treated as `1`: an empty rendering
    /// cannot tell whether results exist.
    pub fn with_max_rows(mut self, max_rows: usize) -> Self {
        self.max_rows = Some(max_rows.max(1));
        self
    }

    /// Renders at most `max_items` items of every nested collection. `0` is treated as `1`.
    pub fn with_max_items(mut self, max_items: usize) -> Self {
        self.max_items = Some(max_items.max(1));
        self
    }

    /// Removes both display limits, keeping the table layout options.
    pub fn with_no_limit(mut self) -> Self {
        self.max_rows = None;
        self.max_items = None;
        self
    }

    /// Whether any display limit is set.
    pub fn is_bounded(&self) -> bool {
        self.max_rows.is_some() || self.max_items.is_some()
    }
}

/// Rendered text together with what the display limits cut from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderedOutput {
    /// The rendered result. Never contains the truncation notice.
    pub text: String,
    /// What was cut; `None` when the text shows the complete result.
    pub truncation: Option<Truncation>,
}

impl RenderedOutput {
    /// Output that shows the complete result.
    pub fn complete(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            truncation: None,
        }
    }

    /// Output with an optional truncation report.
    pub fn new(text: impl Into<String>, truncation: Option<Truncation>) -> Self {
        Self {
            text: text.into(),
            truncation,
        }
    }

    /// The notice to show next to the text, or `None` when nothing was cut. See
    /// [`Truncation::notice`].
    pub fn notice(&self) -> Option<String> {
        self.truncation.as_ref().map(Truncation::notice)
    }

    /// Consumes the output and returns only the text.
    pub fn into_text(self) -> String {
        self.text
    }
}

impl std::fmt::Display for RenderedOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// What the display limits cut from a rendering.
///
/// There is no total count on purpose: counting would cost a full execution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Truncation {
    /// How many top-level results were rendered.
    pub rows_shown: usize,
    /// Whether at least one more top-level result existed.
    pub more_rows: bool,
    /// Whether at least one nested collection was cut to `max_items`.
    pub items_cut: bool,
    /// The nested-collection limit that was applied, when `items_cut` is set.
    pub max_items: Option<usize>,
}

impl Truncation {
    /// Builds a report, or `None` when nothing was cut.
    pub fn of(
        rows_shown: usize,
        more_rows: bool,
        items_cut: bool,
        max_items: Option<usize>,
    ) -> Option<Self> {
        (more_rows || items_cut).then_some(Self {
            rows_shown,
            more_rows,
            items_cut,
            max_items: if items_cut { max_items } else { None },
        })
    }

    /// The one notice text every front end prints after a truncated rendering, one line per kind
    /// of cut. It is never part of the rendered text itself.
    pub fn notice(&self) -> String {
        let mut lines = Vec::with_capacity(2);
        if self.more_rows {
            lines.push(format!(
                "… showing the first {} {}; more exist. Use .limit(n), #{{no_limit: true}}, \
                 /set no-limit (--no-limit), or to_list() for the data.",
                self.rows_shown,
                if self.rows_shown == 1 { "row" } else { "rows" }
            ));
        }
        if self.items_cut {
            let items = match self.max_items {
                Some(n) => format!("{n} {}", if n == 1 { "item" } else { "items" }),
                None => "the item limit".to_string(),
            };
            lines.push(format!(
                "… nested collections were cut to {items}. Use #{{max_items: n}}, \
                 /set max-items <n> (--max-items <n>), or /set no-limit (--no-limit)."
            ));
        }
        lines.join("\n")
    }
}

/// Mark in the rendered name of a display-only step (`limit(101) [display]`, the bound a
/// visualizer appends to the plan): the profile of a displayed run shows it, error locations
/// leave it out.
pub(crate) const DISPLAY_MARK: &str = " [display]";

/// Marker rendered in place of the items a nested collection lost to `max_items`.
pub(crate) const CUT_MARKER: &str = "…";

/// The nested-collection limit of one rendering, remembering whether it cut anything.
pub(crate) struct ItemLimit {
    max: Option<usize>,
    cut: Cell<bool>,
}

impl ItemLimit {
    pub(crate) fn new(max: Option<usize>) -> Self {
        Self {
            max,
            cut: Cell::new(false),
        }
    }

    /// No limit: every item is kept. Used by the script renderer and the tree tests.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn unlimited() -> Self {
        Self::new(None)
    }

    /// How many of `len` items to render; records a cut when that is fewer than `len`.
    pub(crate) fn keep(&self, len: usize) -> usize {
        match self.max {
            Some(max) if len > max => {
                self.cut.set(true);
                max
            }
            _ => len,
        }
    }

    /// Whether a limit is set at all, so unlimited renderings can skip the copying pass.
    pub(crate) fn is_limited(&self) -> bool {
        self.max.is_some()
    }

    pub(crate) fn was_cut(&self) -> bool {
        self.cut.get()
    }
}

/// Copies `prop` with every nested collection cut to the item limit and no marker, for formats
/// whose output is data (JSON), where a sentinel element would break the data's types.
///
/// The top-level value's own entries count as nested: one `group()` row with 100k keys is cut too.
pub(crate) fn cut_nested(prop: &ElementProperty, limit: &ItemLimit) -> ElementProperty {
    match prop {
        ElementProperty::Null => ElementProperty::Null,
        ElementProperty::String(s) => ElementProperty::String(s.clone()),
        ElementProperty::Compressed(text) => ElementProperty::String(text.text()),
        ElementProperty::Deduped(text) => ElementProperty::String(text.as_str().into()),
        ElementProperty::Number(n) => ElementProperty::Number(n.clone()),
        ElementProperty::Boolean(b) => ElementProperty::Boolean(*b),
        ElementProperty::Uuid(u) => ElementProperty::Uuid(*u),
        ElementProperty::Temporal(t) => ElementProperty::Temporal(*t),
        // A vector is one value: never cut (its JSON form is the list of its values).
        ElementProperty::Vector(v) => ElementProperty::Vector(v.clone()),
        ElementProperty::Blob(blob) => ElementProperty::Blob(blob.clone()),
        ElementProperty::Array(items) => {
            let keep = limit.keep(items.len());
            ElementProperty::Array(
                items
                    .iter()
                    .take(keep)
                    .map(|item| cut_nested(item, limit))
                    .collect(),
            )
        }
        ElementProperty::Object(obj) => {
            ElementProperty::Object(Box::new(cut_entries(obj, limit, true)))
        }
        ElementProperty::Map(map) => {
            let keep = limit.keep(map.len());
            ElementProperty::Map(Box::new(
                map.iter()
                    .take(keep)
                    .map(|(key, value)| (key.clone(), cut_nested(value, limit)))
                    .collect(),
            ))
        }
        // A vertex or edge is one row whose properties are its columns: its property set is not a
        // nested collection, but collections inside its property values are.
        ElementProperty::Vertex(vertex) => {
            let mut vertex = vertex.clone();
            *vertex.properties_mut() = cut_entries(vertex.properties(), limit, false);
            ElementProperty::Vertex(vertex)
        }
        ElementProperty::Edge(edge) => {
            let mut edge = edge.clone();
            *edge.properties_mut() = cut_entries(edge.properties(), limit, false);
            ElementProperty::Edge(edge)
        }
    }
}

fn cut_entries(obj: &PropertyMap, limit: &ItemLimit, cut_keys: bool) -> PropertyMap {
    let keep = if cut_keys {
        limit.keep(obj.len())
    } else {
        obj.len()
    };
    obj.iter()
        .take(keep)
        .map(|(key, value)| (key.clone(), cut_nested(value, limit)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_names_rows_and_items() {
        let truncation = Truncation::of(100, true, true, Some(100)).unwrap();
        let notice = truncation.notice();
        assert!(
            notice.starts_with("… showing the first 100 rows; more exist."),
            "{notice}"
        );
        assert!(notice.contains("#{no_limit: true}"), "{notice}");
        assert!(
            notice.contains("… nested collections were cut to 100 items."),
            "{notice}"
        );
        assert_eq!(notice.lines().count(), 2);
    }

    #[test]
    fn nothing_cut_is_no_truncation() {
        assert_eq!(Truncation::of(5, false, false, Some(3)), None);
    }

    #[test]
    fn cut_nested_cuts_at_every_depth() {
        let inner =
            ElementProperty::Array((0..5).map(|i| ElementProperty::from(i as i64)).collect());
        let outer = ElementProperty::Array(vec![inner.clone(), inner.clone(), inner]);
        let limit = ItemLimit::new(Some(2));
        let cut = cut_nested(&outer, &limit);
        assert!(limit.was_cut());
        match cut {
            ElementProperty::Array(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(&items[0], ElementProperty::Array(a) if a.len() == 2));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Error frames leave the display limit out, unless it is the failing step itself.
    #[test]
    fn error_frames_keep_only_a_failing_display_step() {
        use crate::traversal::source::without_display_steps;
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let plan = names(&["v()", "math(\"x\")", "limit(101) [display]"]);
        assert_eq!(
            without_display_steps(plan.clone(), &[1]),
            (names(&["v()", "math(\"x\")"]), vec![1])
        );
        assert_eq!(without_display_steps(plan.clone(), &[2]), (plan, vec![2]));
        let plain = names(&["v()", "limit(3)"]);
        assert_eq!(without_display_steps(plain.clone(), &[1]), (plain, vec![1]));
    }

    /// The display limit is a real `limit()` in the executed plan: the pipeline stops producing
    /// results after `max_rows + 1`, and only those reach materialization.
    #[test]
    fn display_limit_is_in_the_executed_plan() {
        let graph = crate::TraversalGraph::tinkerpop_modern();
        let exec = graph
            .traversal()
            .v(None::<()>)
            .out(None::<()>)
            .with_display_limit(Some(2), true)
            .execute_with(crate::ExecOptions::profile(crate::ProfileType::none()));
        assert_eq!(exec.results().len(), 3);
        let stats = exec.profile().expect("profile");
        let last = stats.metrics().last().expect("steps");
        assert_eq!(last.name(), "limit(3) [display]");
        assert_eq!(last.count_in(), 6);
        assert_eq!(last.counts().traverser_count(), 3);
        let rendered = stats.to_string();
        assert!(rendered.contains("limit(3) [display]"), "{rendered}");
    }

    /// A single-result pipeline keeps its terminal step (so `count_pushdown` still fires) and a
    /// mutating one is never cut short.
    #[test]
    fn no_display_limit_after_barriers_or_mutations() {
        let graph = crate::TraversalGraph::tinkerpop_modern();
        let stats = graph
            .traversal()
            .v(None::<()>)
            .count()
            .with_display_limit(Some(2), true)
            .profile()
            .unwrap();
        assert!(
            stats
                .metrics()
                .iter()
                .all(|s| !s.name().contains("[display]"))
        );
        assert!(
            stats
                .optimizer_rules_applied()
                .iter()
                .any(|r| r == "count_pushdown")
        );

        let mut graph = crate::TraversalGraph::tinkerpop_modern();
        let stats = graph
            .traversal_mut()
            .v(None::<()>)
            .property("seen", true)
            .with_display_limit(Some(2), true)
            .profile()
            .unwrap();
        assert!(
            stats
                .metrics()
                .iter()
                .all(|s| !s.name().contains("[display]"))
        );

        let stats = graph
            .traversal()
            .v(None::<()>)
            .with_display_limit(Some(2), false)
            .profile()
            .unwrap();
        assert!(
            stats
                .metrics()
                .iter()
                .all(|s| !s.name().contains("[display]"))
        );
    }

    #[test]
    fn zero_limits_are_raised_to_one() {
        let options = RenderOptions::new().with_max_rows(0).with_max_items(0);
        assert_eq!(options.max_rows, Some(1));
        assert_eq!(options.max_items, Some(1));
        assert!(!options.with_no_limit().is_bounded());
    }
}
