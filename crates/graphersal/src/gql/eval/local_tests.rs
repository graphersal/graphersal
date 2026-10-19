//! Stage 2c.T2: map projections and list comprehensions in the evaluator (design item 7, M4,
//! E12), on the shared test row of the function tests (`functions::test_env`, its module docs
//! list the graph and the variables).

use super::super::functions::test_env::*;
use super::{Truth, Value};

// ----- list comprehensions -----

/// The `WHERE` of a comprehension keeps an item only when the condition is TRUE: FALSE and
/// `NULL` drop it, whatever the operator gave them.
#[test]
fn a_comprehension_keeps_an_item_only_when_its_condition_is_true() {
    for (condition, kept) in [
        ("true", Truth::True),
        ("false", Truth::False),
        ("null", Truth::Unknown),
    ] {
        let expected = if kept == Truth::True {
            list(vec![int(1)])
        } else {
            list(vec![])
        };
        assert_eq!(value(&format!("[x IN [1] WHERE {condition}]")), expected);
    }
    check_values(&[
        // TRUE, FALSE and NULL items as the condition itself.
        (
            "[x IN [true, false, null] WHERE x]",
            list(vec![boolean(true)]),
        ),
        (
            "[x IN [true, false, null] WHERE NOT x]",
            list(vec![boolean(false)]),
        ),
        // A comparison with a NULL item is NULL: dropped.
        ("[x IN [1, null, 3] WHERE x > 1]", list(vec![int(3)])),
        ("[x IN [1, null, 3] WHERE x IS NULL]", list(vec![null()])),
        ("[x IN [1, null, 3] WHERE x <> 1]", list(vec![int(3)])),
        // Three-valued AND / OR inside.
        (
            "[x IN [1, 2, null] WHERE x > 1 OR true]",
            list(vec![int(1), int(2), null()]),
        ),
        (
            "[x IN [1, 2, null] WHERE x > 1 AND true]",
            list(vec![int(2)]),
        ),
    ]);
}

#[test]
fn a_comprehension_maps_in_list_order() {
    check_values(&[
        (
            "[x IN [3, 1, 2] | x * 10]",
            list(vec![int(30), int(10), int(20)]),
        ),
        (
            "[x IN [3, 1, 2] WHERE x > 1 | x * 10]",
            list(vec![int(30), int(20)]),
        ),
        ("[x IN [3, 1, 2]]", list(vec![int(3), int(1), int(2)])),
        // A NULL item maps like any value.
        (
            "[x IN [1, null] | x IS NULL]",
            list(vec![boolean(false), boolean(true)]),
        ),
        ("[x IN [1, null] | x + 1]", list(vec![int(2), null()])),
        // The map part may ignore the item and read the row.
        (
            "[x IN l WHERE x > 1 | n.name]",
            list(vec![string("marko"), string("marko")]),
        ),
        ("[x IN tail(l) | x + n.age]", list(vec![int(31), int(32)])),
        // A list of elements: the items are elements.
        (
            "[x IN [n, m] | x.name]",
            list(vec![string("marko"), string("vadas")]),
        ),
    ]);
}

#[test]
fn a_null_list_gives_null_and_anything_but_a_list_is_an_error() {
    check_values(&[
        ("[x IN null | x]", null()),
        ("[x IN x | 1]", null()),
        ("[x IN $nothing WHERE x > 1]", null()),
        ("[x IN [] | x]", list(vec![])),
        ("[x IN [] WHERE 1 / 0 = 1]", list(vec![])),
    ]);
    check_statuses(&[
        ("[x IN 5 | x]", "22G03"),
        ("[x IN 'abc' | x]", "22G03"),
        ("[x IN n | x]", "22G03"),
        ("[x IN mp | x]", "22G03"),
        ("[x IN p | x]", "22G03"),
        // A condition that is not a boolean.
        ("[x IN [1] WHERE x]", "22G03"),
        // Errors of the parts surface with their own status.
        ("[x IN [1, 0] | 1 / x]", "22012"),
    ]);
    match eval("[x IN 5 | x]") {
        Err(e) => assert!(
            e.message
                .contains("a list comprehension iterates over a list, not a number"),
            "{}",
            e.message
        ),
        Ok(value) => panic!("{value:?}"),
    }
}

#[test]
fn nested_comprehensions_read_every_variable_around_them() {
    check_values(&[
        (
            "[x IN [1, 2] | [y IN [10, 20] | x + y]]",
            list(vec![
                list(vec![int(11), int(21)]),
                list(vec![int(12), int(22)]),
            ]),
        ),
        (
            "[x IN [[1, 2], [3]] | [y IN x WHERE y > 1]]",
            list(vec![list(vec![int(2)]), list(vec![int(3)])]),
        ),
        // Three levels; the innermost reads the outermost.
        (
            "[a IN [1] | [b IN [2] | [c IN [3] | a * 100 + b * 10 + c]]]",
            list(vec![list(vec![list(vec![int(123)])])]),
        ),
        // A comprehension in the list part of another one is outside its variable.
        (
            "[x IN [y IN l WHERE y > 1] | x * 2]",
            list(vec![int(4), int(6)]),
        ),
        // The binder refuses a repeated name (E14); the evaluator itself binds lexically.
        (
            "[x IN [1, 2] | [x IN [5] | x]]",
            list(vec![list(vec![int(5)]), list(vec![int(5)])]),
        ),
    ]);
}

#[test]
fn a_comprehension_variable_is_no_slot_of_the_row() {
    let mut slots = Vec::new();
    compiled("[x IN l WHERE x > n.age | [y IN [x] | y + m.age]]")
        .unwrap_or_else(|e| panic!("{e:?}"))
        .variables(&mut slots);
    slots.sort_unstable();
    slots.dedup();
    let names: Vec<&str> = slots.iter().map(|slot| VARIABLES[*slot]).collect();
    assert_eq!(names, ["n", "m", "l"]);
    // An unknown name in a part is still an unknown name.
    match compiled("[x IN l | nope]") {
        Err(e) => assert_eq!(e.gqlstatus, "42002"),
        Ok(_) => panic!("compiled"),
    }
}

#[test]
fn a_comprehension_asks_the_row_for_the_results_of_its_parts() {
    // `||` inside the map part reserves through the row around the comprehension.
    match eval_in(&TestRow::with_limit(3), "[x IN ['ab'] | x || 'cd']") {
        Err(e) => assert_eq!(e.gqlstatus, "54001"),
        Ok(value) => panic!("{value:?}"),
    }
    assert_eq!(
        eval_in(&TestRow::with_limit(4), "[x IN ['ab'] | x || 'cd']")
            .unwrap_or_else(|e| panic!("{e:?}")),
        list(vec![string("abcd")])
    );
}

// ----- map projections -----

#[test]
fn a_projection_of_a_node_reads_its_properties() {
    check_values(&[
        (
            "n {.name, .age}",
            map(&[("name", string("marko")), ("age", int(29))]),
        ),
        // Written order, not stored order.
        (
            "n {.age, .name}",
            map(&[("age", int(29)), ("name", string("marko"))]),
        ),
        // A missing property: NULL, the key kept; a stored null reads as NULL.
        (
            "n {.missing, .stored_null}",
            map(&[("missing", null()), ("stored_null", null())]),
        ),
        // `.*`: every stored property in stored order, a stored null left out, no id, no label.
        (
            "n {.*}",
            map(&[
                ("name", string("marko")),
                ("age", int(29)),
                ("tags", list(vec![string("a"), string("b")])),
                ("address", map(&[("city", string("Berlin"))])),
            ]),
        ),
        ("u {.*}", map(&[])),
        ("n {}", map(&[])),
        // An edge.
        ("e {.weight, .*}", map(&[("weight", float(0.5))])),
        // A variable item and an entry; an element inside stays an element.
        (
            "n {.name, m, friend: m.name, e}",
            map(&[
                ("name", string("marko")),
                ("m", Value::Element(El::V(2))),
                ("friend", string("vadas")),
                ("e", Value::Element(El::E(1))),
            ]),
        ),
        // A variable item of a NULL variable.
        ("n {x}", map(&[("x", null())])),
    ]);
}

/// E12: a key written twice keeps its first place and takes the later value, `.*` included.
#[test]
fn a_repeated_key_keeps_its_first_place_and_takes_the_later_value() {
    check_values(&[
        (
            "n {.name, .age, name: 'x'}",
            map(&[("name", string("x")), ("age", int(29))]),
        ),
        (
            "n {name: 'x', .age, .name}",
            map(&[("name", string("marko")), ("age", int(29))]),
        ),
        (
            "n {age: 1, .*}",
            map(&[
                ("age", int(29)),
                ("name", string("marko")),
                ("tags", list(vec![string("a"), string("b")])),
                ("address", map(&[("city", string("Berlin"))])),
            ]),
        ),
        (
            "n {.*, age: 1, name: null}",
            map(&[
                ("name", null()),
                ("age", int(1)),
                ("tags", list(vec![string("a"), string("b")])),
                ("address", map(&[("city", string("Berlin"))])),
            ]),
        ),
        ("mp {.a, a: 2, .a}", map(&[("a", int(1))])),
    ]);
}

#[test]
fn a_projection_of_a_map_reads_its_entries() {
    check_values(&[
        ("mp {.a}", map(&[("a", int(1))])),
        ("mp {.*}", map(&[("a", int(1)), ("b", string("hi"))])),
        (
            "mp {.zz, .*, c: 3}",
            map(&[
                ("zz", null()),
                ("a", int(1)),
                ("b", string("hi")),
                ("c", int(3)),
            ]),
        ),
    ]);
}

/// A NULL target gives NULL (where the record constructor gives a map of NULLs); any other
/// value but a node, an edge or a map is error 22G03.
#[test]
fn a_null_target_gives_null_and_other_values_are_errors() {
    check_values(&[
        ("x {.name}", null()),
        ("x {.*, k: 1}", null()),
        ("{name: x.name}", map(&[("name", null())])),
    ]);
    check_statuses(&[
        ("l {.a}", "22G03"),
        ("p {.a}", "22G03"),
        ("z {.*}", "22G03"),
        // The entries of a projection are evaluated only for a target that has them.
        ("n {k: 1 / 0}", "22012"),
    ]);
    assert_eq!(value("x {k: 1 / 0}"), null());
    match eval("l {.a}") {
        Err(e) => assert!(
            e.message
                .contains("a map projection reads a node, an edge or a map, not a list"),
            "{}",
            e.message
        ),
        Ok(value) => panic!("{value:?}"),
    }
}

#[test]
fn projections_and_comprehensions_combine() {
    check_values(&[
        (
            "[x IN [n, m] | x {.name}]",
            list(vec![
                map(&[("name", string("marko"))]),
                map(&[("name", string("vadas"))]),
            ]),
        ),
        (
            "n {.name, small: [t IN n.tags WHERE t <> 'a']}",
            map(&[
                ("name", string("marko")),
                ("small", list(vec![string("b")])),
            ]),
        ),
        (
            "[x IN [n, x, m] WHERE x {.age}.age > 28 | x.name]",
            list(vec![string("marko")]),
        ),
    ]);
}
