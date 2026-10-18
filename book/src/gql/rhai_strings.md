# Writing GQL in Rhai Strings

In the Gremlin DSL (the command line, the playground, Python scripts, saved queries) a GQL query
is a Rhai string passed to `g.gql(..)`. GQL has quotes and backticks of its own, and Rhai gives
some characters a meaning inside strings, so this page shows which string form to use and how to
write values, names and comments inside it. Through the [MCP tool `gql`](#the-mcp-tool-gql-no-rhai-string-at-all)
none of this applies: the query is plain text.

## Short queries: `"..."` with GQL `'...'` literals

A query on one line goes into a Rhai double-quoted string. Write GQL string literals with
**single quotes**, so nothing needs escaping:

```rhai,graph=modern
g.gql("MATCH (s:software) WHERE s.name = 'lop' RETURN s.lang AS lang").to_list()   // #{"lang": "java"}
```

GQL accepts `"..."` string literals too, but inside a Rhai `"..."` string each one must be
escaped as `\"`:

```rhai,graph=modern
g.gql("MATCH (s:software) WHERE s.name = \"lop\" RETURN s.lang AS lang").to_list()   // #{"lang": "java"}
```

A single quote inside a GQL `'...'` literal is doubled (`'it''s'`), the GQL rule. Rhai processes
backslash escapes in `"..."` strings first, so a backslash that GQL should see is written `\\`.

```rhai,graph=modern
g.gql("MATCH (s:software) RETURN 'it''s' AS t LIMIT 1").to_list()   // #{"t": "it's"}
```

## Long queries: backtick strings

A Rhai backtick string may span lines and processes no backslash escapes, so a multi-line query
reads as it would in any GQL editor. Line breaks are whitespace for GQL; the error positions
count lines and columns inside the query.

```rhai,graph=modern
g.gql(`MATCH (p:person)-[:created]->(s:software)
       WHERE p.age > 30
       RETURN p.name AS who, s.name AS what
       ORDER BY who, what`).to_list()
// #{"what": "lop", "who": "josh"}, #{"what": "ripple", "who": "josh"}, #{"what": "lop", "who": "peter"}
```

Inside a backtick string both GQL quote forms work unescaped (`'lop'` and `"lop"`). Two
characters are special to Rhai there:

- **`${`** starts a Rhai interpolation: `${expr}` is replaced by the value of a Rhai expression.
  GQL never needs `${`, so this only matters when you use it on purpose (next section). A `$`
  followed by a name, a GQL parameter such as `$min`, is left alone.
- **A backtick** ends the string. A backtick that GQL should see is **doubled**: Rhai reads
  ` `` ` inside a backtick string as one backtick. GQL back-quoted identifiers are therefore
  written with doubled backticks there:

```rhai,graph=modern
g.gql(`MATCH (s:software) RETURN s.name AS ``the name`` ORDER BY ``the name```).to_list()   // #{"the name": "lop"}, #{"the name": "ripple"}
```

The same query in a `"..."` string needs no doubling, because a backtick means nothing to a Rhai
double-quoted string:

```rhai,graph=modern
g.gql("MATCH (s:software) RETURN s.name AS `the name` ORDER BY `the name`").to_list()   // #{"the name": "lop"}, #{"the name": "ripple"}
g.gql("MATCH (`my var`:software) RETURN `my var`.name AS n ORDER BY n").to_list()   // #{"n": "lop"}, #{"n": "ripple"}
```

## Values: parameters, not interpolation

Put values into a query as **parameters**: `$name` in the query, the value in the map after it.
A parameter is a typed value that can never change the query text, whatever it contains:

```rhai,graph=modern
g.gql(`MATCH (s:software) WHERE s.lang = $lang RETURN s.name AS name ORDER BY name`, #{lang: "java"}).to_list()   // #{"name": "lop"}, #{"name": "ripple"}
g.gql("MATCH (n:person) WHERE n.age > $min RETURN n.name AS name ORDER BY name", #{min: 30}).to_list()   // #{"name": "josh"}, #{"name": "peter"}
```

Interpolation (`${..}`) and string concatenation (`"..." + x + "..."`) paste the value in as
**query text**. It runs, but a value with a quote in it breaks the query (or changes it), a
number becomes text, and nothing is checked:

```rhai,graph=modern
{ let name = "o'brien"; g.gql(`MATCH (p:person) WHERE p.name = '${name}' RETURN p.age AS age`).to_list() }   // error: unterminated string literal
{ let name = "o'brien"; g.gql(`MATCH (p:person) WHERE p.name = $name RETURN p.age AS age`, #{name: name}).to_list() }   // no rows, no error
```

In the first query the quote in the value ends the GQL string literal early; in the second the
value is compared as it is.

Parameters also keep the plan reusable and let a property index serve the lookup
([Index use](plans.md#index-use)). A parameter name that is not an identifier is back-quoted:
`` $`my value` `` (in a backtick string ` $``my value`` `).

## Comments

GQL has three comment forms, and all of them work inside both Rhai string forms (Rhai does not
look for comments inside a string):

- `// ...` to the end of the line;
- `/* ... */`, not nested;
- `-- ...` to the end of the line, the ISO GQL form.

```rhai,graph=modern
g.gql(`-- people older than 30
MATCH (p:person)            // every person
  WHERE p.age > 30          /* only the older ones */
RETURN p.name AS name       -- one column
ORDER BY name`).to_list()
// #{"name": "josh"}, #{"name": "peter"}
```

**The `--` rule.** `--` is a comment in GQL but an edge in Cypher (`(a)--(b)`). Graphersal
reads `--` as an **edge** when the next character is `>` or `(` (`-->`, `--(`), or when the token
before it is a `)` on the same line (`(a)--(b)`, `(a) -- (b)`); everywhere else it starts a
comment. So a `--` comment on a line of its own or after a value is a comment, and an edge
pattern is never misread. The one trap: a `--` comment written right after a node pattern on the
same line becomes an edge and the query fails loudly; use `//` there.

```rhai,graph=modern
g.gql(`MATCH (a:person)--(b:software) RETURN a.name AS a, b.name AS b ORDER BY a, b`).to_list()
// josh/lop, josh/ripple, marko/lop, peter/lop
```

```rhai,graph=modern
g.gql(`MATCH (p:person) -- every person
       RETURN p.name AS name`)   // error: unexpected `every`: this -- is read as an edge
```

## Which string to use

| Query | Use |
|---|---|
| one line, string literals | `"..."` with GQL `'...'` literals |
| several lines, comments | a backtick string |
| values from your program | `$name` parameters, never `${..}` or `+` |
| a back-quoted GQL name | `"..."` (no doubling), or doubled backticks in a backtick string |

## The MCP tool `gql`: no Rhai string at all

An agent connected to the [dev server's MCP endpoint](../persistence/dev_server.md) can call the
tool `gql` with the query as plain text and the parameters as a JSON object, so none of the
quoting above applies:

```json
{
  "query": "MATCH (p:person)-[:knows]->(f) WHERE p.name = $name RETURN f.name AS friend ORDER BY friend",
  "params": {"name": "marko"},
  "max_rows": 100
}
```

It runs the same `g.gql(..)` call with the parameters as values and returns the rows like the
`query` tool, in written column order. It is a reading tool, also offered on a read-only
server. Prefer it whenever an agent writes GQL: there is no Rhai string to escape and no
interpolation to get wrong. To continue a GQL result with Gremlin steps, use the `query` tool
with `g.gql(..)` in the script.
