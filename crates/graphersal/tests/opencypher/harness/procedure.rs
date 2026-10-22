//! The TCK's test procedures as saved GQL queries (3h.P0 D15).
//!
//! `And there exists a procedure <signature>:` + table declares a procedure whose result is the
//! table: a call returns the output columns of every row whose input columns equal the
//! arguments. The harness registers it, before the query under test runs, as a saved query of
//! the graph's catalog, which GQL's `CALL folder.name(args)` calls:
//!
//! - **Name:** the last segment of the qualified name; the segments before it, joined by `/`,
//!   are the folder (`test.my.proc` -> folder `test/my`, name `proc`; `test.labels` -> folder
//!   `test`).
//! - **Parameters:** one per input, in the signature's order, with the schema of its type
//!   ([`schema_of`]: `STRING` -> `string`, `INTEGER` -> `integer`, `FLOAT` / `NUMBER` ->
//!   `number`, `BOOLEAN` -> `boolean`, `LIST OF ..` -> `array`, `MAP` -> `object`; a trailing
//!   `?` adds `null`). A name the catalog refuses as a parameter name (`in`: a reserved word)
//!   gets `_` appended until it is accepted (`in` -> `in_`); only implicit-argument calls depend
//!   on the names.
//! - **Body** ([`body`]), one GQL query reading the table as a list of maps, every identifier
//!   back-quoted:
//!
//!   ```text
//!   FOR row IN [{`in`: null, `out`: 'nix'}, ..]
//!   FILTER (row.`in` = $in_ OR (row.`in` IS NULL AND $in_ IS NULL)) AND ..
//!   RETURN row.`out` AS `out`, ..
//!   ```
//!
//!   No inputs: no `FILTER`. A `VOID` procedure (outputs `()`) ends in `FINISH` instead of
//!   `RETURN`, so a call yields no columns and no rows. The parameters appear in the body in
//!   signature order, so the first-use order of the body (the saved query's parameter order)
//!   is the signature's.
//!
//! A signature or table the harness cannot express (an unknown type such as `NODE`, a value
//! without a GQL literal) fails the case's setup (class `setup`), never its comparison.

use graphersal::catalog::{Definition, GQL_QUERY_DIALECT, QueryDefinition, QueryParam};
use graphersal::schema::{JsonType, PropertySchema};

use super::gherkin::Table;
use super::values::{TckValue, parse_value};

/// One declared input or output of a procedure.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The name in the signature (and the table header).
    pub name: String,
    /// The type text without the nullability mark (`INTEGER`, `LIST OF STRING`).
    pub ty: String,
    /// Whether the type ends in `?`.
    pub nullable: bool,
}

/// A parsed procedure signature.
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    /// The name's segments (`["test", "my", "proc"]`).
    pub name: Vec<String>,
    /// The inputs in order.
    pub inputs: Vec<Field>,
    /// The outputs in order; `None` for `VOID` (`:: ()`).
    pub outputs: Option<Vec<Field>>,
}

/// Parses `test.my.proc(in :: INTEGER?) :: (out :: STRING?)`; `:: ()` is a `VOID` procedure.
pub fn parse_signature(text: &str) -> Result<Signature, String> {
    let text = text.trim();
    let open = text
        .find('(')
        .ok_or_else(|| format!("signature `{text}`: no `(`"))?;
    let name: Vec<String> = text[..open]
        .trim()
        .split('.')
        .map(|s| s.trim().to_string())
        .collect();
    if name.iter().any(String::is_empty) {
        return Err(format!("signature `{text}`: empty name segment"));
    }
    let rest = &text[open + 1..];
    let close = rest
        .find(')')
        .ok_or_else(|| format!("signature `{text}`: no `)` after the inputs"))?;
    let inputs = fields(&rest[..close], text)?;
    let rest = rest[close + 1..].trim_start();
    let rest = rest
        .strip_prefix("::")
        .ok_or_else(|| format!("signature `{text}`: no `::` before the outputs"))?
        .trim();
    let outputs = rest
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .ok_or_else(|| format!("signature `{text}`: outputs are not in `( .. )`"))?;
    let outputs = if outputs.trim().is_empty() {
        None
    } else {
        Some(fields(outputs, text)?)
    };
    Ok(Signature {
        name,
        inputs,
        outputs,
    })
}

/// `name :: TYPE, ..` (may be empty).
fn fields(list: &str, signature: &str) -> Result<Vec<Field>, String> {
    if list.trim().is_empty() {
        return Ok(Vec::new());
    }
    list.split(',')
        .map(|item| {
            let (name, ty) = item
                .split_once("::")
                .ok_or_else(|| format!("signature `{signature}`: `{item}` has no `::`"))?;
            let name = name.trim();
            let ty = ty.trim();
            if name.is_empty() || ty.is_empty() {
                return Err(format!(
                    "signature `{signature}`: incomplete field `{item}`"
                ));
            }
            let (ty, nullable) = match ty.strip_suffix('?') {
                Some(t) => (t.trim_end(), true),
                None => (ty, false),
            };
            Ok(Field {
                name: name.to_string(),
                ty: ty.to_string(),
                nullable,
            })
        })
        .collect()
}

/// The parameter schema of a TCK type.
pub fn schema_of(field: &Field) -> Result<PropertySchema, String> {
    let words: Vec<&str> = field.ty.split_whitespace().collect();
    let ty = match words.as_slice() {
        ["STRING"] => JsonType::String,
        ["INTEGER"] => JsonType::Integer,
        ["FLOAT"] | ["NUMBER"] => JsonType::Number,
        ["BOOLEAN"] => JsonType::Boolean,
        ["MAP"] => JsonType::Object,
        ["LIST", "OF", ..] | ["LIST?", "OF", ..] => JsonType::Array,
        _ => {
            return Err(format!(
                "unsupported procedure type `{}` of `{}`",
                field.ty, field.name
            ));
        }
    };
    Ok(if field.nullable {
        PropertySchema::of_types(&[ty, JsonType::Null])
    } else {
        PropertySchema::of(ty)
    })
}

/// A back-quoted GQL identifier (a backtick doubled).
fn quoted(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// A TCK value as a GQL literal.
pub fn literal(value: &TckValue) -> Result<String, String> {
    Ok(match value {
        TckValue::Null => "null".to_string(),
        TckValue::Bool(b) => b.to_string(),
        TckValue::Int(i) => i.to_string(),
        TckValue::Float(f) => {
            if !f.is_finite() {
                return Err(format!("no GQL literal for the float {f}"));
            }
            let text = f.to_string();
            if text.contains(['.', 'e', 'E']) {
                text
            } else {
                format!("{text}.0")
            }
        }
        TckValue::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        TckValue::List(items) => {
            let items: Result<Vec<String>, String> = items.iter().map(literal).collect();
            format!("[{}]", items?.join(", "))
        }
        TckValue::Map(map) => {
            let entries: Result<Vec<String>, String> = map
                .iter()
                .map(|(k, v)| Ok(format!("{}: {}", quoted(k), literal(v)?)))
                .collect();
            format!("{{{}}}", entries?.join(", "))
        }
        other => return Err(format!("no GQL literal for the value {other:?}")),
    })
}

/// The parameter names: each input's name, with `_` appended while the catalog refuses it or
/// another input already has it.
pub fn parameter_names(inputs: &[Field]) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(inputs.len());
    for field in inputs {
        let mut name = field.name.clone();
        // Bounded: each round makes the name longer; a refusal for the length ends the loop.
        for _ in 0..8 {
            if !names.contains(&name) && accepted_parameter_name(&name) {
                break;
            }
            name.push('_');
        }
        names.push(name);
    }
    names
}

/// Whether the catalog accepts `name` as a saved query's parameter name.
fn accepted_parameter_name(name: &str) -> bool {
    let probe = QueryDefinition::new("RETURN 1 AS x")
        .with_dialect(GQL_QUERY_DIALECT)
        .with_param(name, QueryParam::new(PropertySchema::any()));
    Definition::query("probe", probe).validate().is_ok()
}

/// The GQL body of the procedure: the table filtered by the inputs, projected to the outputs.
pub fn body(signature: &Signature, table: &Table) -> Result<String, String> {
    let column = |name: &str| {
        table
            .header
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| format!("the procedure table has no column `{name}`"))
    };
    let outputs = signature.outputs.as_deref().unwrap_or(&[]);
    let columns: Vec<(String, usize)> = signature
        .inputs
        .iter()
        .chain(outputs)
        .map(|f| Ok((f.name.clone(), column(&f.name)?)))
        .collect::<Result<_, String>>()?;
    let mut rows = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        let mut entries = Vec::with_capacity(columns.len());
        for (name, at) in &columns {
            let cell = row
                .get(*at)
                .ok_or_else(|| format!("a procedure row has no cell for `{name}`"))?;
            let value = parse_value(cell).map_err(|e| format!("procedure cell `{cell}`: {e}"))?;
            entries.push(format!("{}: {}", quoted(name), literal(&value)?));
        }
        rows.push(format!("{{{}}}", entries.join(", ")));
    }
    let mut text = format!("FOR row IN [{}]", rows.join(", "));
    let params = parameter_names(&signature.inputs);
    if !signature.inputs.is_empty() {
        let conditions: Vec<String> = signature
            .inputs
            .iter()
            .zip(&params)
            .map(|(field, param)| {
                let cell = format!("row.{}", quoted(&field.name));
                let param = format!("${}", quoted(param));
                format!("({cell} = {param} OR ({cell} IS NULL AND {param} IS NULL))")
            })
            .collect();
        text.push_str(" FILTER ");
        text.push_str(&conditions.join(" AND "));
    }
    match &signature.outputs {
        None => text.push_str(" FINISH"),
        Some(outputs) => {
            let items: Vec<String> = outputs
                .iter()
                .map(|f| format!("row.{} AS {}", quoted(&f.name), quoted(&f.name)))
                .collect();
            text.push_str(" RETURN ");
            text.push_str(&items.join(", "));
        }
    }
    Ok(text)
}

/// The saved query of the procedure `signature` (text) with this table.
pub fn definition(signature: &str, table: &Table) -> Result<Definition, String> {
    let parsed = parse_signature(signature)?;
    let text = body(&parsed, table)?;
    let (name, folder) = match parsed.name.split_last() {
        Some((name, folder)) => (name.clone(), folder.join("/")),
        None => return Err(format!("signature `{signature}`: no name")),
    };
    let mut query = QueryDefinition::new(text).with_dialect(GQL_QUERY_DIALECT);
    if !folder.is_empty() {
        query = query.with_folder(folder);
    }
    for (field, param) in parsed.inputs.iter().zip(parameter_names(&parsed.inputs)) {
        query = query.with_param(param, QueryParam::new(schema_of(field)?));
    }
    let definition = Definition::query(name, query);
    definition
        .validate()
        .map_err(|e| format!("procedure `{signature}`: {e}"))?;
    Ok(definition)
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal::catalog::DefinitionBody;

    fn table(header: &[&str], rows: &[&[&str]]) -> Table {
        Table {
            header: header.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect(),
        }
    }

    fn query(definition: &Definition) -> &QueryDefinition {
        match definition.body() {
            DefinitionBody::Query(q) => q,
            other => panic!("not a query: {other:?}"),
        }
    }

    fn param_names(definition: &Definition) -> Vec<String> {
        query(definition)
            .params()
            .iter()
            .map(|(n, _)| n.to_string())
            .collect()
    }

    #[test]
    fn void_procedure_without_inputs() {
        let d = definition("test.doNothing() :: ()", &table(&[], &[])).unwrap();
        assert_eq!(d.name(), "doNothing");
        let q = query(&d);
        assert_eq!(q.folder(), Some("test"));
        assert_eq!(q.dialect(), GQL_QUERY_DIALECT);
        assert_eq!(q.source(), "FOR row IN [] FINISH");
        assert!(q.params().is_empty());
    }

    #[test]
    fn outputs_without_inputs() {
        let d = definition(
            "test.labels() :: (label :: STRING?)",
            &table(&["label"], &[&["'A'"], &["'B'"]]),
        )
        .unwrap();
        assert_eq!(
            query(&d).source(),
            "FOR row IN [{`label`: 'A'}, {`label`: 'B'}] RETURN row.`label` AS `label`"
        );
    }

    #[test]
    fn reserved_input_name_is_renamed_and_null_rows_are_kept() {
        let d = definition(
            "test.my.proc(in :: INTEGER?) :: (out :: STRING?)",
            &table(&["in", "out"], &[&["null", "'nix'"], &["1", "'one'"]]),
        )
        .unwrap();
        assert_eq!(d.name(), "proc");
        let q = query(&d);
        assert_eq!(q.folder(), Some("test/my"));
        assert_eq!(param_names(&d), ["in_"]);
        assert_eq!(
            q.params()[0].1.schema(),
            &PropertySchema::of_types(&[JsonType::Integer, JsonType::Null])
        );
        assert_eq!(
            q.source(),
            "FOR row IN [{`in`: null, `out`: 'nix'}, {`in`: 1, `out`: 'one'}] \
             FILTER (row.`in` = $`in_` OR (row.`in` IS NULL AND $`in_` IS NULL)) \
             RETURN row.`out` AS `out`"
        );
    }

    #[test]
    fn several_inputs_and_outputs_keep_the_signature_order() {
        // The table's columns in another order than the signature: the signature wins.
        let d = definition(
            "test.my.proc(name :: STRING?, id :: INTEGER?) :: (city :: STRING?, country_code :: INTEGER?)",
            &table(
                &["country_code", "city", "id", "name"],
                &[&["46", "'Malmö'", "1", "'Andres'"]],
            ),
        )
        .unwrap();
        assert_eq!(param_names(&d), ["name", "id"]);
        assert_eq!(
            query(&d).source(),
            "FOR row IN [{`name`: 'Andres', `id`: 1, `city`: 'Malmö', `country_code`: 46}] \
             FILTER (row.`name` = $`name` OR (row.`name` IS NULL AND $`name` IS NULL)) \
             AND (row.`id` = $`id` OR (row.`id` IS NULL AND $`id` IS NULL)) \
             RETURN row.`city` AS `city`, row.`country_code` AS `country_code`"
        );
    }

    #[test]
    fn space_before_the_colon_and_numeric_types() {
        let s = parse_signature(
            "test.my.proc(in :: NUMBER?) :: (a :: INTEGER?, b :: FLOAT, c :: LIST? OF STRING?)",
        )
        .unwrap();
        assert_eq!(s.name, ["test", "my", "proc"]);
        let outputs = s.outputs.unwrap();
        assert_eq!(outputs.len(), 3);
        assert!(!outputs[1].nullable);
        assert_eq!(
            schema_of(&s.inputs[0]).unwrap(),
            PropertySchema::of_types(&[JsonType::Number, JsonType::Null])
        );
        assert_eq!(schema_of(&outputs[1]).unwrap(), PropertySchema::number());
        assert_eq!(
            schema_of(&outputs[2]).unwrap(),
            PropertySchema::of_types(&[JsonType::Array, JsonType::Null])
        );
        // The step parser keeps the space before the trailing `:` out of the signature already.
        let d = definition(
            "test.my.proc(in :: INTEGER?) :: (a :: INTEGER?, b :: INTEGER?)",
            &table(&["in", "a", "b"], &[&["null", "1", "2"]]),
        )
        .unwrap();
        assert!(
            query(&d)
                .source()
                .ends_with("RETURN row.`a` AS `a`, row.`b` AS `b`")
        );
    }

    #[test]
    fn root_folder_floats_and_strings() {
        let d = definition(
            "proc(x :: FLOAT?) :: (out :: STRING?)",
            &table(
                &["x", "out"],
                &[&["42.0", "'it\\'s'"], &["1e3", "'a\\\\b'"]],
            ),
        )
        .unwrap();
        assert_eq!(query(&d).folder(), None);
        let source = query(&d).source().to_string();
        assert!(source.contains("`x`: 42.0"), "{source}");
        assert!(source.contains("`x`: 1000.0"), "{source}");
        assert_eq!(
            literal(&TckValue::String("it's".into())).unwrap(),
            "'it\\'s'"
        );
        assert_eq!(literal(&TckValue::Float(42.3)).unwrap(), "42.3");
    }

    #[test]
    fn unsupported_forms_are_setup_errors() {
        assert!(
            definition(
                "test.p(n :: NODE?) :: (x :: STRING?)",
                &table(&["n", "x"], &[])
            )
            .is_err()
        );
        assert!(definition("test.p() :: (x :: STRING?)", &table(&["y"], &[])).is_err());
        assert!(parse_signature("test.p(x) :: ()").is_err());
        assert!(parse_signature("test..p() :: ()").is_err());
        assert!(parse_signature("test.p()").is_err());
    }
}
