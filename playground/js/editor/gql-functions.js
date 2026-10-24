// The GQL functions for the completion of GQL text (query tabs in GQL, the saved-query editor's
// GQL mode; js/editor/gql-completion.js), each with its call form and one short sentence.
//
// The function SET is the engine's: `FUNCTIONS` and `list_function` in
// crates/graphersal/src/gql/parser.rs are the one table the parser, binder and evaluator read.
// tests/gql-functions.test.mjs reads that table from the Rust source and fails when a name (or a
// Cypher alias) is missing here or listed here without being there, so a function added to the
// engine must get its line below. The texts follow the book's tables (book/src/gql/subset.md
// "Aggregation" and "Functions"), which stay the user documentation.

/**
 * `{name, aliases, call, kind, doc}`: `call` the argument form shown and inserted, `kind`
 * 'scalar', 'aggregate' or 'list' (Cypher's list predicates and `reduce`).
 */
export const GQL_FUNCTIONS = Object.freeze([
  // Elements and general
  { name: 'element_id', aliases: ['elementId'], call: '(x)', kind: 'scalar', doc: 'The id of a vertex or edge, as a string.' },
  { name: 'labels', aliases: [], call: '(x)', kind: 'scalar', doc: "A vertex's labels in stored order (primary first); an edge's label as a one-item list." },
  { name: 'type', aliases: [], call: '(e)', kind: 'scalar', doc: "An edge's label (for a vertex use labels(v))." },
  { name: 'coalesce', aliases: [], call: '(a, b, ...)', kind: 'scalar', doc: 'The first argument that is not NULL.' },
  { name: 'nullif', aliases: [], call: '(a, b)', kind: 'scalar', doc: 'NULL when a = b is TRUE, else a.' },
  { name: 'path_length', aliases: ['length'], call: '(p)', kind: 'scalar', doc: 'The number of edges of a path.' },
  // Aggregates
  { name: 'count', aliases: [], call: '(x)', kind: 'aggregate', doc: 'count(*): the number of rows; count(x): the rows where x is not NULL; count(DISTINCT x) each value once.' },
  { name: 'sum', aliases: [], call: '(x)', kind: 'aggregate', doc: 'The sum of the numbers (integers stay integers); 0 on no input.' },
  { name: 'avg', aliases: [], call: '(x)', kind: 'aggregate', doc: 'The mean, always a float; NULL on no input.' },
  { name: 'min', aliases: [], call: '(x)', kind: 'aggregate', doc: 'The smallest value in the order of ORDER BY; NULL on no input.' },
  { name: 'max', aliases: [], call: '(x)', kind: 'aggregate', doc: 'The largest value in the order of ORDER BY; NULL on no input.' },
  { name: 'collect_list', aliases: ['collect'], call: '(x)', kind: 'aggregate', doc: 'The values as a list ([] on no input).' },
  { name: 'percentile_cont', aliases: ['percentileCont'], call: '(x, p)', kind: 'aggregate', doc: 'The percentile p (0.0 to 1.0) of the numbers, interpolated; a float, NULL on no input.' },
  { name: 'percentile_disc', aliases: ['percentileDisc'], call: '(x, p)', kind: 'aggregate', doc: 'The percentile p (0.0 to 1.0) of the numbers: the nearest value itself; NULL on no input.' },
  // Conversions
  { name: 'cast', aliases: [], call: '(x AS INTEGER)', kind: 'scalar', doc: 'Strict conversion to INTEGER, FLOAT, STRING or BOOLEAN: text that is no value of the type is an error (22018).' },
  { name: 'toInteger', aliases: [], call: '(x)', kind: 'scalar', doc: 'Lenient conversion to an integer: unconvertible text gives NULL; also true / false and Cypher literals.' },
  { name: 'toIntegerOrNull', aliases: [], call: '(x)', kind: 'scalar', doc: 'As toInteger, and NULL for a value of an unsupported kind.' },
  { name: 'toFloat', aliases: [], call: '(x)', kind: 'scalar', doc: 'Lenient conversion to a float: unconvertible text gives NULL.' },
  { name: 'toFloatOrNull', aliases: [], call: '(x)', kind: 'scalar', doc: 'As toFloat, and NULL for a value of an unsupported kind.' },
  { name: 'toBoolean', aliases: [], call: '(x)', kind: 'scalar', doc: 'Lenient conversion to a boolean (integers: 0 is FALSE, any other TRUE).' },
  { name: 'toBooleanOrNull', aliases: [], call: '(x)', kind: 'scalar', doc: 'As toBoolean, and NULL for a value of an unsupported kind.' },
  { name: 'toString', aliases: [], call: '(x)', kind: 'scalar', doc: "The text of a value, as CAST(x AS STRING) (a float keeps its .0; a date or time in Neo4j's text, '12:00')." },
  { name: 'toStringOrNull', aliases: [], call: '(x)', kind: 'scalar', doc: 'As toString, and NULL for a value of an unsupported kind.' },
  // Strings
  { name: 'upper', aliases: ['toUpper'], call: '(s)', kind: 'scalar', doc: 'The string in upper case.' },
  { name: 'lower', aliases: ['toLower'], call: '(s)', kind: 'scalar', doc: 'The string in lower case.' },
  { name: 'char_length', aliases: ['character_length'], call: '(s)', kind: 'scalar', doc: 'The number of characters.' },
  { name: 'byte_length', aliases: ['octet_length'], call: '(s)', kind: 'scalar', doc: 'The number of UTF-8 bytes.' },
  { name: 'left', aliases: [], call: '(s, n)', kind: 'scalar', doc: 'The first n characters (the whole string when n is larger).' },
  { name: 'right', aliases: [], call: '(s, n)', kind: 'scalar', doc: 'The last n characters (the whole string when n is larger).' },
  { name: 'substring', aliases: [], call: '(s, start, length)', kind: 'scalar', doc: 'length characters from the 0-based start (to the end without length); past the end an empty string.' },
  { name: 'trim', aliases: [], call: '(s)', kind: 'scalar', doc: 'The string without spaces at both ends; TRIM([LEADING | TRAILING | BOTH] [c] FROM s) removes the character c; trim(list, n) drops the last n items.' },
  { name: 'btrim', aliases: [], call: '(s, chars)', kind: 'scalar', doc: 'The string without any character of the set chars (default: the space) at both ends.' },
  { name: 'ltrim', aliases: [], call: '(s, chars)', kind: 'scalar', doc: 'The string without any character of the set chars (default: the space) at the start.' },
  { name: 'rtrim', aliases: [], call: '(s, chars)', kind: 'scalar', doc: 'The string without any character of the set chars (default: the space) at the end.' },
  { name: 'replace', aliases: [], call: '(s, search, replacement)', kind: 'scalar', doc: 'Every occurrence of search replaced, left to right (an optional fourth argument limits how many).' },
  { name: 'split', aliases: [], call: '(s, delimiter)', kind: 'scalar', doc: 'The list of the parts between the delimiters (a string or a list of strings); empty parts are kept.' },
  { name: 'reverse', aliases: [], call: '(x)', kind: 'scalar', doc: 'A string with its characters reversed, or a list with its items reversed.' },
  { name: 'isEmpty', aliases: [], call: '(x)', kind: 'scalar', doc: 'Whether a string, list or map has no character, item or entry.' },
  // Numbers
  { name: 'abs', aliases: [], call: '(x)', kind: 'scalar', doc: 'The absolute value, of the argument\'s type.' },
  { name: 'floor', aliases: [], call: '(x)', kind: 'scalar', doc: 'Rounded down; an integer stays the integer.' },
  { name: 'ceil', aliases: ['ceiling'], call: '(x)', kind: 'scalar', doc: 'Rounded up; an integer stays the integer.' },
  { name: 'round', aliases: [], call: '(x)', kind: 'scalar', doc: "Rounded, always a float; round(x, precision, mode) with a mode such as 'HALF_EVEN'." },
  { name: 'sign', aliases: [], call: '(x)', kind: 'scalar', doc: '-1, 0 or 1, an integer.' },
  { name: 'sqrt', aliases: [], call: '(x)', kind: 'scalar', doc: 'The square root, a float (of a negative number an error, 2201F).' },
  { name: 'exp', aliases: [], call: '(x)', kind: 'scalar', doc: 'e to the power x, a float.' },
  { name: 'ln', aliases: [], call: '(x)', kind: 'scalar', doc: 'The natural logarithm, a float (of a number <= 0 an error, 2201E).' },
  { name: 'log', aliases: [], call: '(base, x)', kind: 'scalar', doc: 'The logarithm to base, a float; log(x) is the natural logarithm.' },
  { name: 'log10', aliases: [], call: '(x)', kind: 'scalar', doc: 'The decimal logarithm, a float.' },
  { name: 'power', aliases: [], call: '(x, y)', kind: 'scalar', doc: 'x to the power y, a float.' },
  { name: 'mod', aliases: [], call: '(a, b)', kind: 'scalar', doc: 'The remainder, exactly as a % b.' },
  // Lists, elements and paths
  { name: 'size', aliases: [], call: '(list)', kind: 'scalar', doc: 'The number of items of a list (size(s) of a string: its characters).' },
  { name: 'cardinality', aliases: [], call: '(list)', kind: 'scalar', doc: 'The number of items of a list.' },
  { name: 'head', aliases: [], call: '(list)', kind: 'scalar', doc: 'The first item; NULL for [].' },
  { name: 'last', aliases: [], call: '(list)', kind: 'scalar', doc: 'The last item; NULL for [].' },
  { name: 'tail', aliases: [], call: '(list)', kind: 'scalar', doc: 'All items but the first.' },
  { name: 'range', aliases: [], call: '(start, end)', kind: 'scalar', doc: 'The integers from start to end inclusive (an optional third argument is the step).' },
  { name: 'keys', aliases: [], call: '(x)', kind: 'scalar', doc: 'The property keys of a vertex or edge, or the keys of a map.' },
  { name: 'properties', aliases: [], call: '(x)', kind: 'scalar', doc: 'The properties of a vertex or edge as a map.' },
  { name: 'startNode', aliases: [], call: '(e)', kind: 'scalar', doc: 'The stored source vertex of the edge.' },
  { name: 'endNode', aliases: [], call: '(e)', kind: 'scalar', doc: 'The stored target vertex of the edge.' },
  { name: 'nodes', aliases: [], call: '(p)', kind: 'scalar', doc: 'The vertices of a path, in the path\'s order.' },
  { name: 'relationships', aliases: [], call: '(p)', kind: 'scalar', doc: 'The edges of a path, in the path\'s order.' },
  // Dates and times (Cypher's temporal functions; book subset.md "Dates and times")
  { name: 'date', aliases: [], call: "('2015-07-21')", kind: 'scalar', doc: 'A date from ISO 8601 text, a map ({year, month, day}, {year, week, dayOfWeek}, ...) or another temporal value; no argument: today (UTC).' },
  { name: 'local_time', aliases: ['localtime'], call: "('12:31:14')", kind: 'scalar', doc: 'A time of day without an offset, from text, a map ({hour, minute, second, ...}) or another temporal value; no argument: now.' },
  { name: 'local_datetime', aliases: ['localdatetime'], call: "('2015-07-21T21:40')", kind: 'scalar', doc: 'A date and time without an offset, from text, a map or another temporal value; no argument: now.' },
  { name: 'zoned_datetime', aliases: ['datetime'], call: "('2015-07-21T21:40+01:00')", kind: 'scalar', doc: 'An instant with its UTC offset (and the zone name it was built with: 2015-07-21T21:40[Europe/London]), from text, a map with timezone or another temporal value; no argument: now (UTC).' },
  { name: 'zoned_time', aliases: ['time'], call: "('12:31:14+01:00')", kind: 'scalar', doc: 'A time of day with its UTC offset (none: UTC), from text, a map with timezone or another temporal value; no argument: now (UTC).' },
  { name: 'duration', aliases: [], call: "('P14DT16H12M')", kind: 'scalar', doc: 'A duration from ISO 8601 text or a map ({years, months, weeks, days, hours, ...}, fractions allowed).' },
  { name: 'current_date', aliases: [], call: '()', kind: 'scalar', doc: 'The date the query started (UTC); ISO GQL writes it without parentheses: CURRENT_DATE.' },
  { name: 'current_time', aliases: [], call: '()', kind: 'scalar', doc: 'The time of day the query started, with its offset (UTC); also CURRENT_TIME without parentheses.' },
  { name: 'current_timestamp', aliases: [], call: '()', kind: 'scalar', doc: 'The instant the query started (UTC); also CURRENT_TIMESTAMP without parentheses.' },
  { name: 'local_timestamp', aliases: [], call: '()', kind: 'scalar', doc: 'The local date and time the query started; also LOCAL_TIMESTAMP without parentheses.' },
  { name: 'date.statement', aliases: [], call: '()', kind: 'scalar', doc: 'The date the query started (one instant per query); a zone argument gives it in that zone.' },
  { name: 'date.transaction', aliases: [], call: '()', kind: 'scalar', doc: 'As date.statement().' },
  { name: 'localtime.statement', aliases: [], call: '()', kind: 'scalar', doc: 'The time of day the query started.' },
  { name: 'localtime.transaction', aliases: [], call: '()', kind: 'scalar', doc: 'As localtime.statement().' },
  { name: 'localdatetime.statement', aliases: [], call: '()', kind: 'scalar', doc: 'The local date and time the query started.' },
  { name: 'localdatetime.transaction', aliases: [], call: '()', kind: 'scalar', doc: 'As localdatetime.statement().' },
  { name: 'datetime.statement', aliases: [], call: '()', kind: 'scalar', doc: 'The instant the query started (UTC, or in a zone given as the argument).' },
  { name: 'datetime.transaction', aliases: [], call: '()', kind: 'scalar', doc: 'As datetime.statement().' },
  { name: 'time.statement', aliases: [], call: '()', kind: 'scalar', doc: 'The time of day the query started, with its offset (UTC, or in a zone given as the argument).' },
  { name: 'time.transaction', aliases: [], call: '()', kind: 'scalar', doc: 'As time.statement().' },
  { name: 'date.truncate', aliases: [], call: "('month', d)", kind: 'scalar', doc: 'The date cut to the start of a unit (millennium .. day; 2017 to millennium gives 2000); an optional map overrides fields.' },
  { name: 'localtime.truncate', aliases: [], call: "('hour', t)", kind: 'scalar', doc: 'The local time cut to the start of a unit (day .. nanosecond); an optional map overrides fields.' },
  { name: 'localdatetime.truncate', aliases: [], call: "('day', x)", kind: 'scalar', doc: 'The local datetime cut to the start of a unit (millennium .. nanosecond); an optional map overrides fields.' },
  { name: 'datetime.truncate', aliases: [], call: "('day', x)", kind: 'scalar', doc: 'The datetime cut to the start of a unit, keeping its offset and zone name; an optional map overrides fields (timezone too).' },
  { name: 'time.truncate', aliases: [], call: "('hour', t)", kind: 'scalar', doc: 'The time cut to the start of a unit (day .. nanosecond), keeping its offset; an optional map overrides fields (timezone too).' },
  { name: 'datetime.fromepoch', aliases: [], call: '(seconds, nanoseconds)', kind: 'scalar', doc: 'The datetime (UTC) of a time since 1970-01-01T00:00Z.' },
  { name: 'datetime.fromepochmillis', aliases: [], call: '(milliseconds)', kind: 'scalar', doc: 'The datetime (UTC) of milliseconds since 1970-01-01T00:00Z.' },
  { name: 'duration.between', aliases: [], call: '(a, b)', kind: 'scalar', doc: 'The duration from a to b: whole months, then days, then the time, each signed.' },
  { name: 'duration.inMonths', aliases: [], call: '(a, b)', kind: 'scalar', doc: 'The whole months from a to b, as a duration.' },
  { name: 'duration.inDays', aliases: [], call: '(a, b)', kind: 'scalar', doc: 'The whole days from a to b, as a duration.' },
  { name: 'duration.inSeconds', aliases: [], call: '(a, b)', kind: 'scalar', doc: 'The elapsed time from a to b, as a duration of seconds only.' },
  // Cypher's list predicates and reduce (they bind a variable inside the call)
  { name: 'all', aliases: [], call: '(x IN list WHERE cond)', kind: 'list', doc: 'TRUE when the condition holds for every item.' },
  { name: 'any', aliases: [], call: '(x IN list WHERE cond)', kind: 'list', doc: 'TRUE when the condition holds for at least one item.' },
  { name: 'none', aliases: [], call: '(x IN list WHERE cond)', kind: 'list', doc: 'TRUE when the condition holds for no item.' },
  { name: 'single', aliases: [], call: '(x IN list WHERE cond)', kind: 'list', doc: 'TRUE when the condition holds for exactly one item.' },
  { name: 'reduce', aliases: [], call: '(acc = init, x IN list | expr)', kind: 'list', doc: 'Folds the list: acc starts at init and becomes expr for each item.' },
]);

/** Every spelling of every function (names and aliases), as written in the table. */
export function gqlFunctionSpellings() {
  return GQL_FUNCTIONS.flatMap((f) => [f.name, ...f.aliases]);
}
