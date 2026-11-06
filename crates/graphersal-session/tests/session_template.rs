//! `Session::template`: sessions on other threads serve the same graph (the same `Arc`) with
//! their own engine and cancellation token, and share the force-layout jobs.

use std::sync::Arc;

use graphersal_session::{RunOptions, Session, SessionPresets, SessionTemplate};
use serde_json::Value;

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn a_template_is_send_and_sync() {
    assert_send_sync::<SessionTemplate>();
}

#[test]
fn sessions_of_a_template_serve_the_same_graph_on_other_threads() {
    let mut first = Session::with_presets(SessionPresets::new().with_script_timeout_ms(Some(5000)));
    first.load_sample("modern").unwrap();
    let template = first.template().unwrap();
    let graph = first.graph().clone();
    let answers: Vec<(bool, Value, Value)> = (0..3)
        .map(|_| {
            let template = template.clone();
            let graph = graph.clone();
            std::thread::spawn(move || {
                let mut session = template.session().unwrap();
                let same = Arc::ptr_eq(session.graph(), &graph);
                let answer: Value = serde_json::from_str(
                    &session.execute("g.v().count().next()", &RunOptions::default()),
                )
                .unwrap();
                (same, answer, session.presets().to_value())
            })
        })
        .map(|thread| thread.join().unwrap())
        .collect();
    for (same, answer, presets) in answers {
        assert!(same, "a template's session serves the same graph");
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(presets["scriptTimeoutMs"], 5000, "{presets}");
    }
    assert_eq!(
        first.info().unwrap()["name"],
        template.session().unwrap().info().unwrap()["name"]
    );
}

#[test]
fn a_cancel_token_of_one_session_does_not_reach_another() {
    let mut first = Session::new();
    first.load_sample("modern").unwrap();
    let mut second = first.template().unwrap().session().unwrap();
    first.cancel_token().cancel();
    let answer: Value =
        serde_json::from_str(&second.execute("g.v().count().next()", &RunOptions::default()))
            .unwrap();
    assert_eq!(answer["ok"], true, "{answer}");
}

#[test]
fn layout_jobs_are_shared_and_their_limit_is_raised() {
    let mut first = Session::new();
    first.load_sample("modern").unwrap();
    let mut second = first.template().unwrap().session().unwrap();
    let request =
        r#"{"vertexCount": 3, "edgeSource": [0, 1], "edgeTarget": [1, 2], "iterations": 500}"#;
    let started = first.layout_start_json(request, 0.0).unwrap();
    let job = started["job"].as_u64().unwrap();
    // The job started through the first session continues through the second.
    let stepped = second.layout_step(job, 50.0).unwrap();
    assert_eq!(stepped["job"], job, "{stepped}");
    // Four jobs by default: a fifth forgets the first; with a higher limit it is kept.
    first.set_layout_job_limit(8).unwrap();
    for seed in 0..5 {
        let request = format!(
            r#"{{"vertexCount": 3, "edgeSource": [0, 1], "edgeTarget": [1, 2], "seed": {}, "iterations": 500}}"#,
            seed + 10
        );
        second.layout_start_json(&request, 0.0).unwrap();
    }
    assert!(
        second.layout_step(job, 1.0).is_ok(),
        "the first job is still kept"
    );
}

/// With read-only runs the sessions of a template run their queries at the same time; a run
/// unit would let one thread at a time hold the graph.
#[test]
fn read_only_runs_of_two_sessions_overlap() {
    let mut first = Session::new();
    first.load_sample("modern").unwrap();
    first.set_read_only_runs(true).unwrap();
    let template = first.template().unwrap();
    let script = "let s = 0; for i in 0..400000 { s += i; } g.v().count().next() + s";
    let started = std::time::Instant::now();
    first.execute(script, &RunOptions::default());
    let single = started.elapsed();
    let started = std::time::Instant::now();
    let runs: Vec<_> = (0..2)
        .map(|_| {
            let template = template.clone();
            std::thread::spawn(move || {
                let mut session = template.session().unwrap();
                let answer: Value =
                    serde_json::from_str(&session.execute(script, &RunOptions::default()))
                        .unwrap();
                answer["ok"] == true
            })
        })
        .collect();
    for run in runs {
        assert!(run.join().unwrap());
    }
    let both = started.elapsed();
    assert!(
        both < single * 9 / 5,
        "two runs took {both:?}, one {single:?}"
    );
}
