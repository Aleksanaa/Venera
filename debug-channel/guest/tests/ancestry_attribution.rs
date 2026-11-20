#[path = "../src/ancestry.rs"]
mod ancestry;

use ancestry::{Attribution, Checkpoint, Lifecycle};
use serde_json::json;
use venera_debug_protocol::{matches_filter, Filter, LogRecord, ProcessIdentity, Target};

const BOOT: u64 = 100;

fn instance(pid: u32, start: u64) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        start_time_filetime: Some(start.to_string()),
        boot_time_filetime: Some(BOOT.to_string()),
        start_key: Some((start * 1000 + u64::from(pid)).to_string()),
        ..Default::default()
    }
}

fn record(pid: u32) -> LogRecord {
    LogRecord {
        stream_id: "test".into(),
        seq: 1,
        observed_at: "2026-11-20T00:00:00Z".into(),
        source_time: None,
        source: "etw".into(),
        component: "guest".into(),
        level: "info".into(),
        raw_level: "info".into(),
        session_id: None,
        process: Some(ProcessIdentity {
            pid,
            ..Default::default()
        }),
        ancestors: Vec::new(),
        tid: None,
        text: "historical record".into(),
        structured: None,
    }
}

fn attributed(checkpoint: &Checkpoint, pid: u32, time: u64) -> LogRecord {
    let mut row = record(pid);
    assert_eq!(
        checkpoint.attribute_record(&mut row, BOOT, time, false),
        Attribution::Ready
    );
    row
}

fn matches(row: &LogRecord, instance: &ProcessIdentity, include_descendants: bool) -> bool {
    let filter = Filter {
        targets: vec![Target::ProcessInstance {
            pid: instance.pid,
            start_time_filetime: instance.start_time_filetime.clone().unwrap(),
            include_descendants,
            boot_time_filetime: instance.boot_time_filetime.clone(),
            start_key: instance.start_key.clone(),
        }],
        ..Default::default()
    };
    matches_filter(&filter, row, &row.ancestors)
}

#[test]
fn historical_attribution_across_seventeen_interruptions() {
    let mut checkpoint = Checkpoint::default();
    assert!(checkpoint.processes.started(instance(10, 110), 0, None));
    for index in 0..17 {
        let from = 120 + 10 * index;
        assert!(checkpoint.processes.apply(Lifecycle::Gap {
            from,
            through: from + 2
        }));
        assert!(checkpoint
            .processes
            .started(instance(10, from + 3), 0, None));
    }
    let restored: Checkpoint =
        serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    for model in [&checkpoint, &restored] {
        for index in 0..17 {
            let from = 120 + 10 * index;
            let previous_start = if index == 0 { 110 } else { from - 7 };
            let before = attributed(model, 10, from - 1);
            assert_eq!(before.process, Some(instance(10, previous_start)));
            for time in from..=from + 2 {
                let mut row = record(10);
                assert_eq!(
                    model.attribute_record(&mut row, BOOT, time, false),
                    Attribution::Missing(10),
                    "source time {time}"
                );
            }
            assert_eq!(
                attributed(model, 10, from + 3).process,
                Some(instance(10, from + 3))
            );
        }
    }
}

#[test]
fn legacy_checkpoint_retirement_boundary_applies_to_attribution() {
    let restore = |processes| -> Checkpoint {
        serde_json::from_value(json!({"processes": processes, "through_filetime": 200})).unwrap()
    };
    let instances = json!([
        {"identity": instance(10, 110), "parent": null, "session": null, "ended": null},
        {"identity": instance(20, 150), "parent": null, "session": null, "ended": null}
    ]);
    let legacy = restore(instances.clone());
    assert_eq!(
        attributed(&legacy, 10, 119).process,
        Some(instance(10, 110))
    );
    let checkpoint = restore(json!({
        "instances": instances, "interruptions": [], "retired_before": 150
    }));
    assert_eq!(
        checkpoint.attribute_record(&mut record(10), BOOT, 119, false),
        Attribution::Missing(10)
    );
    assert_eq!(
        attributed(&checkpoint, 20, 150).process,
        Some(instance(20, 150))
    );
}

#[test]
fn distant_ancestors_match_descendant_filters() {
    let mut checkpoint = Checkpoint::default();
    for pid in 1..=131 {
        assert!(checkpoint
            .processes
            .started(instance(pid, 110 + u64::from(pid)), pid - 1, None));
    }
    let row = attributed(&checkpoint, 131, 250);
    assert_eq!(row.process, Some(instance(131, 241)));
    let expected: Vec<_> = (1..=130)
        .rev()
        .map(|pid| instance(pid, 110 + u64::from(pid)))
        .collect();
    assert_eq!(row.ancestors, expected);
    assert!(matches(&row, &instance(1, 111), true));
    assert!(!matches(&row, &instance(1, 111), false));
}

#[test]
fn a_cycle_visits_each_ancestor_once() {
    let mut checkpoint = Checkpoint::default();
    assert!(checkpoint.processes.started(instance(10, 110), 20, None));
    assert!(checkpoint.processes.started(instance(20, 110), 10, None));
    assert!(checkpoint.processes.started(instance(30, 120), 10, None));

    let member = attributed(&checkpoint, 10, 115);
    assert_eq!(member.ancestors, [instance(20, 110)]);
    let descendant = attributed(&checkpoint, 30, 130);
    assert_eq!(descendant.ancestors, [instance(10, 110), instance(20, 110)]);
    assert!(matches(&descendant, &instance(20, 110), true));
}

#[test]
fn reused_pid_instances_keep_their_historical_descendants() {
    let mut checkpoint = Checkpoint::default();
    let old = instance(10, 110);
    let child = instance(20, 120);
    let reused = instance(10, 140);
    assert!(checkpoint.processes.started(old.clone(), 0, None));
    assert!(checkpoint.processes.started(child.clone(), 10, None));
    checkpoint.processes.ended(10, 110, 130);
    assert!(checkpoint.processes.started(reused.clone(), 20, None));
    assert!(checkpoint.processes.started(instance(30, 150), 10, None));

    let late = attributed(&checkpoint, 10, 125);
    assert_eq!(late.process, Some(old.clone()));
    assert!(matches(&late, &old, false));
    assert!(!matches(&late, &reused, true));
    let old_child = attributed(&checkpoint, 20, 145);
    assert!(matches(&old_child, &old, true));
    assert!(!matches(&old_child, &reused, true));
    let new_child = attributed(&checkpoint, 30, 155);
    assert_eq!(new_child.ancestors, [reused.clone(), child, old.clone()]);
    assert!(matches(&new_child, &reused, true));
    assert!(matches(&new_child, &old, true));
    let current = attributed(&checkpoint, 10, 155);
    assert_eq!(current.process, Some(reused.clone()));
    assert!(matches(&current, &reused, false));
    assert!(!matches(&current, &old, false));
}
