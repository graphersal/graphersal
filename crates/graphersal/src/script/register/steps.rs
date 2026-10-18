//! Registration of the step builder methods (both spellings, every arity) including the
//! positional variadic overloads.

use super::Registry;
use crate::script::*;

/// Registers every step builder.
pub(super) fn register(registry: &mut Registry) {
    registry
        .register_fn_as(&["V", "v"], ScriptGraphTraversalSource::v_empty)
        .register_fn_as(&["V", "v"], ScriptGraphTraversalSource::v_single)
        .register_fn_as(&["V", "v"], ScriptGraphTraversalSource::v)
        .register_fn_as(&["E", "e"], ScriptGraphTraversalSource::e_empty)
        .register_fn_as(&["E", "e"], ScriptGraphTraversalSource::e_single)
        .register_fn_as(&["E", "e"], ScriptGraphTraversalSource::e)
        .register_fn("count", ScriptGraphTraversalSource::count)
        .register_fn("limit", ScriptGraphTraversalSource::limit)
        .register_fn("label", ScriptGraphTraversalSource::label)
        .register_fn("labels", ScriptGraphTraversalSource::labels)
        .register_fn("id", ScriptGraphTraversalSource::id)
        .register_fn("identity", ScriptGraphTraversalSource::identity)
        .register_fn("sack", ScriptGraphTraversalSource::sack)
        .register_fn("sack", ScriptGraphTraversalSource::sack_with)
        .register_fn_as(
            &["withSack", "with_sack"],
            ScriptGraphTraversalSource::with_sack,
        )
        .register_fn_as(
            &["withSack", "with_sack"],
            ScriptGraphTraversalSource::with_sack_merge,
        )
        .register_fn_as(
            &["withBulk", "with_bulk"],
            ScriptGraphTraversalSource::with_bulk,
        )
        .register_fn_as(
            &["withPath", "with_path"],
            ScriptGraphTraversalSource::with_path,
        )
        .register_fn("inject", ScriptGraphTraversalSource::inject_empty)
        .register_fn("inject", ScriptGraphTraversalSource::inject_single)
        .register_fn("cap", ScriptGraphTraversalSource::cap_empty)
        .register_fn("cap", ScriptGraphTraversalSource::cap_single)
        .register_fn("cap", ScriptGraphTraversalSource::cap)
        .register_fn("aggregate", ScriptGraphTraversalSource::aggregate)
        .register_fn("store", ScriptGraphTraversalSource::store)
        .register_fn("tree", ScriptGraphTraversalSource::tree_empty)
        .register_fn("tree", ScriptGraphTraversalSource::tree_single)
        .register_fn("fold", ScriptGraphTraversalSource::fold)
        .register_fn("fold", ScriptGraphTraversalSource::fold_with)
        .register_fn("unfold", ScriptGraphTraversalSource::unfold)
        .register_fn("group", ScriptGraphTraversalSource::group)
        .register_fn("group", ScriptGraphTraversalSource::group_side_effect)
        .register_fn_as(
            &["group_count", "groupCount"],
            ScriptGraphTraversalSource::group_count,
        )
        .register_fn_as(
            &["group_count", "groupCount"],
            ScriptGraphTraversalSource::group_count_side_effect,
        )
        .register_fn("order", ScriptGraphTraversalSource::order)
        .register_fn("order", ScriptGraphTraversalSource::order_scoped)
        .register_fn("by", ScriptGraphTraversalSource::by_empty)
        .register_fn("by", ScriptGraphTraversalSource::by)
        .register_fn("by", ScriptGraphTraversalSource::by_with_order)
        .register_fn("max", ScriptGraphTraversalSource::max)
        .register_fn("mean", ScriptGraphTraversalSource::mean)
        .register_fn("median", ScriptGraphTraversalSource::median)
        .register_fn("min", ScriptGraphTraversalSource::min)
        .register_fn("sum", ScriptGraphTraversalSource::sum)
        .register_fn("count", ScriptGraphTraversalSource::count_scoped)
        .register_fn("sum", ScriptGraphTraversalSource::sum_scoped)
        .register_fn("min", ScriptGraphTraversalSource::min_scoped)
        .register_fn("max", ScriptGraphTraversalSource::max_scoped)
        .register_fn("mean", ScriptGraphTraversalSource::mean_scoped)
        .register_fn("coin", ScriptGraphTraversalSource::coin)
        .register_fn("coin", ScriptGraphTraversalSource::coin_int)
        .register_fn("fail", ScriptGraphTraversalSource::fail)
        .register_fn("fail", ScriptGraphTraversalSource::fail_with)
        .register_fn("sample", ScriptGraphTraversalSource::sample)
        .register_fn("sample", ScriptGraphTraversalSource::sample_scoped)
        .register_fn_as(
            &["has_id", "hasId"],
            ScriptGraphTraversalSource::has_id_single,
        )
        .register_fn_as(&["has_id", "hasId"], ScriptGraphTraversalSource::has_id)
        .register_fn_as(
            &["has_key", "hasKey"],
            ScriptGraphTraversalSource::has_key_single,
        )
        .register_fn_as(&["has_key", "hasKey"], ScriptGraphTraversalSource::has_key)
        .register_fn_as(
            &["has_label", "hasLabel"],
            ScriptGraphTraversalSource::has_any_label,
        )
        .register_fn_as(
            &["has_label", "hasLabel"],
            ScriptGraphTraversalSource::has_label_single,
        )
        .register_fn_as(
            &["has_label", "hasLabel"],
            ScriptGraphTraversalSource::has_label,
        )
        .register_fn_as(
            &["has_value", "hasValue"],
            ScriptGraphTraversalSource::has_value,
        )
        .register_fn_as(
            &["has_value", "has_value_any", "hasValue"],
            ScriptGraphTraversalSource::has_value_multi,
        )
        // `has_value_p`/`has_p` (like `has_value_any`, `where_p`, `where_t` below): explicit
        // Graphersal forms of the overloaded `hasValue`/`has`/`where`, not TinkerPop steps, so
        // there is no Gremlin camelCase spelling; registered once on purpose.
        .register_fn("has_value_p", ScriptGraphTraversalSource::has_value_p)
        .register_fn("has_p", ScriptGraphTraversalSource::has_p)
        .register_fn("has", ScriptGraphTraversalSource::has)
        .register_fn("has", ScriptGraphTraversalSource::has_t)
        .register_fn("has", ScriptGraphTraversalSource::has_property)
        .register_fn("has", ScriptGraphTraversalSource::has_labeled)
        .register_fn_as(&["has_not", "hasNot"], ScriptGraphTraversalSource::has_not)
        .register_fn("discard", ScriptGraphTraversalSource::discard)
        .register_fn("dedup", ScriptGraphTraversalSource::dedup)
        .register_fn("barrier", ScriptGraphTraversalSource::barrier)
        .register_fn("barrier", ScriptGraphTraversalSource::barrier_sized)
        .register_fn("dedup", ScriptGraphTraversalSource::dedup_scoped)
        .register_fn("dedup", ScriptGraphTraversalSource::dedup_label)
        .register_fn("skip", ScriptGraphTraversalSource::skip)
        .register_fn("tail", ScriptGraphTraversalSource::tail_empty)
        .register_fn("tail", ScriptGraphTraversalSource::tail)
        .register_fn("range", ScriptGraphTraversalSource::range)
        .register_fn("limit", ScriptGraphTraversalSource::limit_scoped)
        .register_fn("skip", ScriptGraphTraversalSource::skip_scoped)
        .register_fn("tail", ScriptGraphTraversalSource::tail_scoped_default)
        .register_fn("tail", ScriptGraphTraversalSource::tail_scoped)
        .register_fn("range", ScriptGraphTraversalSource::range_scoped)
        .register_fn("values", ScriptGraphTraversalSource::values_empty)
        .register_fn("values", ScriptGraphTraversalSource::values_single)
        .register_fn("values", ScriptGraphTraversalSource::values)
        .register_fn("project", ScriptGraphTraversalSource::project_single)
        .register_fn("project", ScriptGraphTraversalSource::project)
        .register_fn_as(
            &["element_map", "elementMap"],
            ScriptGraphTraversalSource::element_map_empty,
        )
        .register_fn_as(
            &["element_map", "elementMap"],
            ScriptGraphTraversalSource::element_map_single,
        )
        .register_fn_as(
            &["element_map", "elementMap"],
            ScriptGraphTraversalSource::element_map,
        )
        .register_fn("properties", ScriptGraphTraversalSource::properties_empty)
        .register_fn("properties", ScriptGraphTraversalSource::properties_single)
        .register_fn("properties", ScriptGraphTraversalSource::properties)
        .register_fn("value", ScriptGraphTraversalSource::value)
        .register_fn("key", ScriptGraphTraversalSource::key)
        .register_fn_as(
            &["value_map", "valueMap"],
            ScriptGraphTraversalSource::value_map_empty,
        )
        .register_fn("math", ScriptGraphTraversalSource::math)
        .register_fn("replace", ScriptGraphTraversalSource::replace)
        .register_fn("replace", ScriptGraphTraversalSource::replace_scoped)
        .register_fn("out", ScriptGraphTraversalSource::out_empty)
        .register_fn("out", ScriptGraphTraversalSource::out_single)
        .register_fn("out", ScriptGraphTraversalSource::out)
        .register_fn_as(&["in", "in_"], ScriptGraphTraversalSource::in_empty)
        .register_fn_as(&["in", "in_"], ScriptGraphTraversalSource::in_single)
        .register_fn_as(&["in", "in_"], ScriptGraphTraversalSource::in_)
        .register_fn("element", ScriptGraphTraversalSource::element)
        .register_fn("both", ScriptGraphTraversalSource::both_empty)
        .register_fn("both", ScriptGraphTraversalSource::both_single)
        .register_fn("both", ScriptGraphTraversalSource::both)
        .register_fn_as(
            &["both_e", "bothE"],
            ScriptGraphTraversalSource::both_e_empty,
        )
        .register_fn_as(
            &["both_e", "bothE"],
            ScriptGraphTraversalSource::both_e_single,
        )
        .register_fn_as(&["both_e", "bothE"], ScriptGraphTraversalSource::both_e)
        .register_fn_as(&["in_e", "inE"], ScriptGraphTraversalSource::in_e_empty)
        .register_fn_as(&["in_e", "inE"], ScriptGraphTraversalSource::in_e_single)
        .register_fn_as(&["in_e", "inE"], ScriptGraphTraversalSource::in_e)
        .register_fn_as(&["in_v", "inV"], ScriptGraphTraversalSource::in_v)
        .register_fn_as(&["out_e", "outE"], ScriptGraphTraversalSource::out_e_empty)
        .register_fn_as(&["out_e", "outE"], ScriptGraphTraversalSource::out_e_single)
        .register_fn_as(&["out_e", "outE"], ScriptGraphTraversalSource::out_e)
        .register_fn_as(&["out_v", "outV"], ScriptGraphTraversalSource::out_v)
        .register_fn_as(&["both_v", "bothV"], ScriptGraphTraversalSource::both_v)
        .register_fn_as(&["other_v", "otherV"], ScriptGraphTraversalSource::other_v)
        .register_fn("constant", ScriptGraphTraversalSource::constant)
        .register_fn("combine", ScriptGraphTraversalSource::combine)
        .register_fn("conjoin", ScriptGraphTraversalSource::conjoin)
        .register_fn("concat", ScriptGraphTraversalSource::concat_empty)
        .register_fn("concat", ScriptGraphTraversalSource::concat_single)
        .register_fn("format", ScriptGraphTraversalSource::format)
        .register_fn("split", ScriptGraphTraversalSource::split)
        .register_fn("split", ScriptGraphTraversalSource::split_scoped)
        .register_fn("reverse", ScriptGraphTraversalSource::reverse)
        .register_fn("index", ScriptGraphTraversalSource::index)
        .register_fn("difference", ScriptGraphTraversalSource::difference)
        .register_fn("disjunct", ScriptGraphTraversalSource::disjunct)
        .register_fn("intersect", ScriptGraphTraversalSource::intersect)
        .register_fn("merge", ScriptGraphTraversalSource::merge)
        .register_fn("product", ScriptGraphTraversalSource::product)
        .register_fn("length", ScriptGraphTraversalSource::length)
        .register_fn("length", ScriptGraphTraversalSource::length_scoped)
        .register_fn("trim", ScriptGraphTraversalSource::trim)
        .register_fn("trim", ScriptGraphTraversalSource::trim_scoped)
        .register_fn_as(&["l_trim", "lTrim"], ScriptGraphTraversalSource::l_trim)
        .register_fn_as(
            &["l_trim", "lTrim"],
            ScriptGraphTraversalSource::l_trim_scoped,
        )
        .register_fn_as(&["r_trim", "rTrim"], ScriptGraphTraversalSource::r_trim)
        .register_fn_as(
            &["r_trim", "rTrim"],
            ScriptGraphTraversalSource::r_trim_scoped,
        )
        .register_fn_as(
            &["to_lower", "toLower"],
            ScriptGraphTraversalSource::to_lower,
        )
        .register_fn_as(
            &["to_lower", "toLower"],
            ScriptGraphTraversalSource::to_lower_scoped,
        )
        .register_fn_as(
            &["to_upper", "toUpper"],
            ScriptGraphTraversalSource::to_upper,
        )
        .register_fn_as(
            &["to_upper", "toUpper"],
            ScriptGraphTraversalSource::to_upper_scoped,
        )
        .register_fn_as(
            &["as_string", "asString"],
            ScriptGraphTraversalSource::as_string,
        )
        .register_fn_as(
            &["as_string", "asString"],
            ScriptGraphTraversalSource::as_string_with,
        )
        .register_fn_as(
            &["as_string", "asString"],
            ScriptGraphTraversalSource::as_string_scoped,
        )
        .register_fn_as(&["as_bool", "asBool"], ScriptGraphTraversalSource::as_bool)
        .register_fn_as(
            &["as_bool", "asBool"],
            ScriptGraphTraversalSource::as_bool_with,
        )
        .register_fn_as(
            &["as_number", "asNumber"],
            ScriptGraphTraversalSource::as_number,
        )
        .register_fn_as(
            &["as_number", "asNumber"],
            ScriptGraphTraversalSource::as_number_t,
        )
        .register_fn_as(
            &["as_number", "asNumber"],
            ScriptGraphTraversalSource::as_number_with,
        )
        .register_fn("cast", ScriptGraphTraversalSource::cast)
        // The date steps (book part "Dates and Times").
        .register_fn_as(&["as_date", "asDate"], ScriptGraphTraversalSource::as_date)
        .register_fn_as(
            &["as_date", "asDate"],
            ScriptGraphTraversalSource::as_date_t,
        )
        .register_fn_as(
            &["as_duration", "asDuration"],
            ScriptGraphTraversalSource::as_duration,
        )
        // The vector steps (book page "Vectors").
        .register_fn_as(
            &["as_vector", "asVector"],
            ScriptGraphTraversalSource::as_vector,
        )
        .register_fn("quantize", ScriptGraphTraversalSource::quantize)
        .register_fn("similarity", ScriptGraphTraversalSource::similarity)
        .register_fn("similarity", ScriptGraphTraversalSource::similarity_2)
        .register_fn("similarity", ScriptGraphTraversalSource::similarity_3)
        .register_fn("nearest", ScriptGraphTraversalSource::nearest)
        .register_fn("nearest", ScriptGraphTraversalSource::nearest_with)
        .register_fn("embed", ScriptGraphTraversalSource::embed)
        .register_fn("embed", ScriptGraphTraversalSource::embed_1)
        .register_fn("embed", ScriptGraphTraversalSource::embed_2)
        .register_fn_as(
            &["vector_mean", "vectorMean"],
            ScriptGraphTraversalSource::vector_mean,
        )
        .register_fn_as(
            &["date_add", "dateAdd"],
            ScriptGraphTraversalSource::date_add,
        )
        .register_fn_as(
            &["date_add", "dateAdd"],
            ScriptGraphTraversalSource::date_add_in,
        )
        .register_fn_as(
            &["date_add", "dateAdd"],
            ScriptGraphTraversalSource::date_add_duration,
        )
        .register_fn_as(
            &["date_add", "dateAdd"],
            ScriptGraphTraversalSource::date_add_duration_in,
        )
        .register_fn_as(
            &["date_diff", "dateDiff"],
            ScriptGraphTraversalSource::date_diff,
        )
        .register_fn_as(
            &["date_diff", "dateDiff"],
            ScriptGraphTraversalSource::date_diff_unit,
        )
        .register_fn_as(
            &["date_diff", "dateDiff"],
            ScriptGraphTraversalSource::date_diff_unit_in,
        )
        .register_fn("age", ScriptGraphTraversalSource::age)
        .register_fn("age", ScriptGraphTraversalSource::age_in)
        .register_fn_as(
            &["date_trunc", "dateTrunc"],
            ScriptGraphTraversalSource::date_trunc,
        )
        .register_fn_as(
            &["date_trunc", "dateTrunc"],
            ScriptGraphTraversalSource::date_trunc_in,
        )
        .register_fn_as(
            &["date_part", "datePart"],
            ScriptGraphTraversalSource::date_part,
        )
        .register_fn_as(
            &["date_part", "datePart"],
            ScriptGraphTraversalSource::date_part_in,
        )
        .register_fn_as(&["at_zone", "atZone"], ScriptGraphTraversalSource::at_zone)
        .register_fn_as(
            &["as_local", "asLocal"],
            ScriptGraphTraversalSource::as_local,
        )
        .register_fn_as(
            &["as_local", "asLocal"],
            ScriptGraphTraversalSource::as_local_in,
        )
        .register_fn("cast", ScriptGraphTraversalSource::cast_with)
        .register_fn("substring", ScriptGraphTraversalSource::substring)
        .register_fn("substring", ScriptGraphTraversalSource::substring_from)
        .register_fn(
            "substring",
            ScriptGraphTraversalSource::substring_scoped_from,
        )
        .register_fn("substring", ScriptGraphTraversalSource::substring_scoped)
        .register_fn_as(&["as", "as_"], ScriptGraphTraversalSource::as_single)
        .register_fn_as(&["as", "as_"], ScriptGraphTraversalSource::as_)
        .register_fn("select", ScriptGraphTraversalSource::select_single)
        .register_fn("select", ScriptGraphTraversalSource::select)
        .register_fn("select", ScriptGraphTraversalSource::select_column)
        .register_fn("path", ScriptGraphTraversalSource::path)
        .register_fn_as(
            &["simple_path", "simplePath"],
            ScriptGraphTraversalSource::simple_path,
        )
        .register_fn_as(
            &["cyclic_path", "cyclicPath"],
            ScriptGraphTraversalSource::cyclic_path,
        )
        .register_fn_as(&["is", "is_"], ScriptGraphTraversalSource::is)
        .register_fn("where", ScriptGraphTraversalSource::where_step)
        .register_fn("where", ScriptGraphTraversalSource::where_label_p)
        .register_fn("where_p", ScriptGraphTraversalSource::where_p)
        .register_fn("where_t", ScriptGraphTraversalSource::where_t)
        // The creating steps carry the evaluation's `now()` (`defaultFunction` `now`/`today`).
        .register_fn_as(
            &["addV", "add_v"],
            |ctx: NativeCallContext, this: &mut ScriptGraphTraversalSource| {
                this.add_v_empty().stamped(&ctx)
            },
        )
        .register_fn_as(
            &["addV", "add_v"],
            |ctx: NativeCallContext, this: &mut ScriptGraphTraversalSource, label: String| {
                this.add_v(label).stamped(&ctx)
            },
        )
        .register_fn_as(
            &["addV", "add_v"],
            |ctx: NativeCallContext,
             this: &mut ScriptGraphTraversalSource,
             labels: Vec<Dynamic>| {
                this.add_v_labels(labels).map(|source| source.stamped(&ctx))
            },
        )
        .register_fn_as(
            &["addV", "add_v"],
            |ctx: NativeCallContext,
             this: &mut ScriptGraphTraversalSource,
             label: ScriptGraphTraversalSource| {
                this.add_v_traversal(label).stamped(&ctx)
            },
        )
        .register_fn_as(
            &["addE", "add_e"],
            |ctx: NativeCallContext, this: &mut ScriptGraphTraversalSource| {
                this.add_e_empty().stamped(&ctx)
            },
        )
        .register_fn_as(
            &["addE", "add_e"],
            |ctx: NativeCallContext, this: &mut ScriptGraphTraversalSource, label: Dynamic| {
                this.add_e(label).map(|source| source.stamped(&ctx))
            },
        )
        .register_fn_as(
            &["withSideEffect", "with_side_effect"],
            ScriptGraphTraversalSource::with_side_effect,
        )
        .register_fn_as(
            &["withSideEffect", "with_side_effect"],
            ScriptGraphTraversalSource::with_side_effect_reduced,
        )
        .register_fn("and", ScriptGraphTraversalSource::and_infix)
        .register_fn("and", ScriptGraphTraversalSource::and_one)
        .register_fn("not", ScriptGraphTraversalSource::not)
        .register_fn("filter", ScriptGraphTraversalSource::filter_t)
        .register_fn("or", ScriptGraphTraversalSource::or_infix)
        .register_fn("or", ScriptGraphTraversalSource::or_one)
        .register_fn("all", ScriptGraphTraversalSource::all)
        .register_fn("any", ScriptGraphTraversalSource::any)
        .register_fn("none", ScriptGraphTraversalSource::none)
        .register_fn("coalesce", ScriptGraphTraversalSource::coalesce_one)
        .register_fn("union", ScriptGraphTraversalSource::union_empty)
        .register_fn("union", ScriptGraphTraversalSource::union_one)
        .register_fn("optional", ScriptGraphTraversalSource::optional)
        .register_fn("local", ScriptGraphTraversalSource::local)
        .register_fn("map", ScriptGraphTraversalSource::map)
        .register_fn_as(
            &["flat_map", "flatMap"],
            ScriptGraphTraversalSource::flat_map,
        )
        .register_fn_as(
            &["sideEffect", "side_effect"],
            ScriptGraphTraversalSource::side_effect,
        )
        .register_fn("choose", ScriptGraphTraversalSource::choose3)
        .register_fn("choose", ScriptGraphTraversalSource::choose2)
        .register_fn("choose", ScriptGraphTraversalSource::choose_switch)
        .register_fn("branch", ScriptGraphTraversalSource::branch)
        .register_fn("option", ScriptGraphTraversalSource::option)
        .register_fn(
            "option",
            ScriptGraphTraversalSource::option_merge_cardinality,
        )
        .register_fn_as(
            &["mergeV", "merge_v"],
            |ctx: NativeCallContext, this: &mut ScriptGraphTraversalSource| {
                this.merge_v_empty().stamped(&ctx)
            },
        )
        .register_fn_as(&["mergeV", "merge_v"], ScriptGraphTraversalSource::merge_v)
        .register_fn_as(
            &["mergeE", "merge_e"],
            |ctx: NativeCallContext, this: &mut ScriptGraphTraversalSource| {
                this.merge_e_empty().stamped(&ctx)
            },
        )
        .register_fn_as(&["mergeE", "merge_e"], ScriptGraphTraversalSource::merge_e)
        // .register_fn("add_v_", ScriptGraphTraversalSource::add_v_)
        .register_fn("property", ScriptGraphTraversalSource::property)
        .register_fn("property", ScriptGraphTraversalSource::property_cardinality)
        .register_fn("property", ScriptGraphTraversalSource::property_map)
        .register_fn("property", ScriptGraphTraversalSource::property_unit)
        .register_fn(
            "property",
            ScriptGraphTraversalSource::property_cardinality_map,
        )
        .register_fn(
            "property",
            ScriptGraphTraversalSource::property_cardinality_unit,
        )
        .register_fn("property", ScriptGraphTraversalSource::property_token)
        .register_fn_as(
            &["json_path", "jsonPath"],
            ScriptGraphTraversalSource::json_path,
        )
        .register_fn_as(
            &["glob_path", "globPath"],
            ScriptGraphTraversalSource::glob_path,
        )
        .register_fn_as(
            &["glob_path", "globPath"],
            ScriptGraphTraversalSource::glob_path_with,
        );

    // Looping: the snake_case and camelCase spellings coincide.
    registry
        .register_fn("repeat", ScriptGraphTraversalSource::repeat)
        .register_fn("repeat", ScriptGraphTraversalSource::repeat_named)
        .register_fn("times", ScriptGraphTraversalSource::times)
        .register_fn("until", ScriptGraphTraversalSource::until)
        .register_fn("emit", ScriptGraphTraversalSource::emit_empty)
        .register_fn("emit", ScriptGraphTraversalSource::emit)
        .register_fn("loops", ScriptGraphTraversalSource::loops_empty)
        .register_fn("loops", ScriptGraphTraversalSource::loops);

    #[cfg(feature = "serde")]
    registry.register_fn_as(
        &["property_json", "propertyJson"],
        ScriptGraphTraversalSource::property_json,
    );

    // One word: snake_case and camelCase are the same spelling.
    #[cfg(feature = "gql")]
    registry
        .register_fn_as(&["gql"], ScriptGraphTraversalSource::gql)
        .register_fn_as(&["gql"], ScriptGraphTraversalSource::gql_with);

    registry
        .register_fn("from", ScriptGraphTraversalSource::from_single)
        .register_fn("from", ScriptGraphTraversalSource::from_step)
        .register_fn("to", ScriptGraphTraversalSource::to_single)
        .register_fn("to", ScriptGraphTraversalSource::to)
        .register_fn("to", ScriptGraphTraversalSource::to_direction)
        .register_fn_as(&["toE", "to_e"], ScriptGraphTraversalSource::to_e)
        .register_fn_as(&["toV", "to_v"], ScriptGraphTraversalSource::to_v)
        .register_fn("drop", ScriptGraphTraversalSource::drop)
        .register_fn_as(
            &["removeProperty", "remove_property"],
            ScriptGraphTraversalSource::remove_property_empty,
        )
        .register_fn_as(
            &["removeProperty", "remove_property"],
            ScriptGraphTraversalSource::remove_property,
        )
        .register_fn_as(
            &["addLabel", "add_label"],
            ScriptGraphTraversalSource::add_label,
        )
        .register_fn_as(
            &["dropLabel", "drop_label"],
            ScriptGraphTraversalSource::drop_label,
        )
        .register_fn_as(
            &["setLabel", "set_label"],
            ScriptGraphTraversalSource::set_label,
        )
        .register_fn("subgraph", ScriptGraphTraversalSource::subgraph)
        .register_fn("subgraph", ScriptGraphTraversalSource::subgraph_no_label);

    // `valueMap(..)` names its own spelling in diagnostics, so each spelling gets its closure.
    for name in ["value_map", "valueMap"] {
        registry
            .register_fn(
                name,
                move |s: &mut ScriptGraphTraversalSource, arg: Dynamic| {
                    s.value_map_single(name, arg)
                },
            )
            .register_fn(
                name,
                move |s: &mut ScriptGraphTraversalSource, arg: Vec<Dynamic>| s.value_map(name, arg),
            );
    }

    // True positional variadic overloads (arities 2..=10, see `register_variadic!`'s doc comment)
    // for every TinkerPop `T... varargs` step this DSL wraps in a `Vec<Dynamic>`/array-literal
    // form, in every spelling the step already has. `hasLabel("a", "b")` now behaves identically
    // to the pre-existing `hasLabel(["a", "b"])`; the array-literal form is unchanged and remains
    // the unlimited-arity escape hatch. `from`/`to` are deliberately excluded: real TinkerPop's
    // `from(String)`/`to(String)` (and their `Traversal` overloads) are single-argument, not
    // varargs, so there is no TinkerPop-compliant positional form to add for them.
    register_variadic!(
        ctx registry,
        ["V", "v"],
        |ctx, s: &mut ScriptGraphTraversalSource, args| ScriptGraphTraversalSource::v(ctx, s, args)
    );
    register_variadic!(
        ctx registry,
        ["E", "e"],
        |ctx, s: &mut ScriptGraphTraversalSource, args| ScriptGraphTraversalSource::e(ctx, s, args)
    );
    // `inject` has no array-literal escape hatch: a Rhai array is one injected list value
    // (TinkerPop's `inject([1, 2])`), so more than ten values need several `inject()` calls.
    register_variadic!(
        ctx registry,
        ["concat"],
        |ctx, s: &mut ScriptGraphTraversalSource, args| ScriptGraphTraversalSource::concat(
            ctx, s, args
        )
    );
    register_variadic!(
        ctx registry,
        ["inject"],
        |ctx, s: &mut ScriptGraphTraversalSource, args| ScriptGraphTraversalSource::inject(
            ctx, s, args
        )
    );
    register_variadic!(
        registry,
        ["cap"],
        |s: &mut ScriptGraphTraversalSource, args| s.cap(args)
    );
    register_variadic!(
        ctx registry,
        ["has_id", "hasId"],
        |ctx, s: &mut ScriptGraphTraversalSource, args| {
            ScriptGraphTraversalSource::has_id(ctx, s, args)
        }
    );
    register_variadic!(
        registry,
        ["has_key", "hasKey"],
        |s: &mut ScriptGraphTraversalSource, args| s.has_key(args)
    );
    register_variadic!(
        registry,
        ["has_label", "hasLabel"],
        |s: &mut ScriptGraphTraversalSource, args| s.has_label(args)
    );
    register_variadic!(
        ctx registry,
        ["has_value", "has_value_any", "hasValue"],
        |ctx, s: &mut ScriptGraphTraversalSource, args| {
            ScriptGraphTraversalSource::has_value_multi(ctx, s, args)
        }
    );
    register_variadic!(
        registry,
        ["values"],
        |s: &mut ScriptGraphTraversalSource, args| s.values(args)
    );
    register_variadic!(
        registry,
        ["project"],
        |s: &mut ScriptGraphTraversalSource, args| s.project(args)
    );
    register_variadic!(
        registry,
        ["element_map", "elementMap"],
        |s: &mut ScriptGraphTraversalSource, args| s.element_map(args)
    );
    register_variadic!(
        registry,
        ["properties"],
        |s: &mut ScriptGraphTraversalSource, args| s.properties(args)
    );
    register_variadic!(registry, ["value_map", "valueMap"] => name, |s: &mut ScriptGraphTraversalSource, args| s.value_map(name, args));
    register_variadic!(
        registry,
        ["out"],
        |s: &mut ScriptGraphTraversalSource, args| s.out(args)
    );
    register_variadic!(
        registry,
        ["in", "in_"],
        |s: &mut ScriptGraphTraversalSource, args| s.in_(args)
    );
    register_variadic!(
        registry,
        ["both"],
        |s: &mut ScriptGraphTraversalSource, args| s.both(args)
    );
    register_variadic!(
        registry,
        ["both_e", "bothE"],
        |s: &mut ScriptGraphTraversalSource, args| s.both_e(args)
    );
    register_variadic!(
        registry,
        ["in_e", "inE"],
        |s: &mut ScriptGraphTraversalSource, args| s.in_e(args)
    );
    register_variadic!(
        registry,
        ["out_e", "outE"],
        |s: &mut ScriptGraphTraversalSource, args| s.out_e(args)
    );
    register_variadic!(
        registry,
        ["as", "as_"],
        |s: &mut ScriptGraphTraversalSource, args| s.as_(args)
    );
    register_variadic!(
        registry,
        ["select"],
        |s: &mut ScriptGraphTraversalSource, args| s.select(args)
    );
    register_variadic!(
        registry,
        ["and"],
        |s: &mut ScriptGraphTraversalSource, args| s.and(args)
    );
    register_variadic!(
        registry,
        ["or"],
        |s: &mut ScriptGraphTraversalSource, args| s.or(args)
    );
    register_variadic!(
        registry,
        ["coalesce"],
        |s: &mut ScriptGraphTraversalSource, args| s.coalesce(args)
    );
    register_variadic!(
        registry,
        ["union"],
        |s: &mut ScriptGraphTraversalSource, args| s.union(args)
    );
    // `dedup(Scope, label, ...)` / `dedup(label, label, ...)`: arity 2..=10, so it never shadows
    // the one-argument `dedup(Scope)` / `dedup(label)`.
    register_variadic!(
        registry,
        ["dedup"],
        |s: &mut ScriptGraphTraversalSource, args| s.dedup_scoped_labels(args)
    );
    // `to(Direction, label, ...)` / `toE(Direction, label, ...)`: arity 2..=10, so they never
    // shadow the one-argument add-edge `to(String|traversal)` or `to(Direction)`.
    register_variadic!(registry, ["to"] => name, |s: &mut ScriptGraphTraversalSource, args| s.direction_args_step(name, args, DirectionStepKind::Vertex));
    register_variadic!(registry, ["toE", "to_e"] => name, |s: &mut ScriptGraphTraversalSource, args| s.direction_args_step(name, args, DirectionStepKind::Edge));
}
