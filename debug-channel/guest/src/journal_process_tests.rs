use crate::ancestry::Lifecycle;
use venera_debug_protocol::ProcessIdentity;

const BOOT: u64 = 134_393_633_875_000_000;
const NOW: u64 = 1_794_889_800_000;

#[test]
fn late_gap_filters_inferred_parent_before_and_after_retirement() {
    for retire in [false, true] {
        for gap_before_retirement in [false, true] {
            let mut processes = crate::ancestry::Processes::default();
            processes.apply(start(10, 10, Some(11), 0));
            processes.apply(start(20, 30, Some(22), 10));
            assert_eq!(
                processes.ancestors(&identity(20, 30, Some(22)), 0),
                [identity(10, 10, Some(11))]
            );
            if retire && !gap_before_retirement {
                assert!(processes.evict_oldest());
            }
            processes.apply(Lifecycle::Gap {
                from: BOOT + 20,
                through: BOOT + 25,
            });
            if retire && gap_before_retirement {
                assert!(processes.evict_oldest());
            }
            let checkpoint = crate::ancestry::Checkpoint {
                processes,
                through_filetime: BOOT + 40,
            };
            let dir = Directory::new();
            let mut journal = dir.open(RetentionPolicy::default());
            journal.discover_imports(vec![source()]).unwrap();
            drop(journal);
            let path = dir.0.join("metadata.json");
            let mut metadata: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            metadata["processes"][BOOT.to_string()] = serde_json::to_value(checkpoint).unwrap();
            fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
            let journal = dir.open(RetentionPolicy::default());
            let checkpoint = journal.process_checkpoint(BOOT).unwrap();
            let mut child = record(20, Some(22));
            assert!(checkpoint.attribute_record(&mut child, BOOT, BOOT + 40, true));
            assert_eq!(child.process, Some(identity(20, 30, Some(22))));
            assert_eq!(
                child.ancestors,
                [],
                "retire={retire}, gap_before_retirement={gap_before_retirement}"
            );
            let filter = venera_debug_protocol::Filter {
                targets: vec![venera_debug_protocol::Target::ProcessInstance {
                    pid: 10,
                    start_time_filetime: (BOOT + 10).to_string(),
                    include_descendants: true,
                    boot_time_filetime: Some(BOOT.to_string()),
                    start_key: Some("11".into()),
                }],
                ..Default::default()
            };
            assert!(!venera_debug_protocol::matches_filter(
                &filter,
                &child,
                &child.ancestors
            ));
        }
    }
}

#[test]
fn frozen_parent_keeps_valid_intervals_and_explicit_parent_key_evidence() {
    for parent_key in [None, Some(11)] {
        let mut processes = crate::ancestry::Processes::default();
        processes.apply(start(10, 10, Some(11), 0));
        processes.apply(Lifecycle::Start {
            identity: identity(20, 30, Some(22)),
            parent: 10,
            parent_key,
            session: Some(1),
        });
        assert!(processes.evict_oldest());
        let processes: crate::ancestry::Processes =
            serde_json::from_slice(&serde_json::to_vec(&processes).unwrap()).unwrap();
        assert_eq!(
            processes.ancestors(&identity(20, 30, Some(22)), 0),
            [identity(10, 10, Some(11))]
        );
        let mut processes = processes;
        processes.apply(Lifecycle::Gap {
            from: BOOT + 40,
            through: BOOT + 45,
        });
        assert_eq!(
            processes.ancestors(&identity(20, 30, Some(22)), 0),
            [identity(10, 10, Some(11))]
        );
        if parent_key.is_some() {
            processes.apply(Lifecycle::Gap {
                from: BOOT + 20,
                through: BOOT + 25,
            });
            let mut processes: crate::ancestry::Processes =
                serde_json::from_slice(&serde_json::to_vec(&processes).unwrap()).unwrap();
            processes.apply(start(10, 10, Some(99), 0));
            assert_eq!(
                processes.ancestors(&identity(20, 30, Some(22)), 0),
                [identity(10, 10, Some(11))]
            );
        }
    }
}

#[test]
fn replay_before_persisted_discontinuity_restores_the_historical_interval() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 200,
            NOW,
        )
        .unwrap();
    journal
        .checkpoint_processes(&source(), vec![Lifecycle::Discontinuity], BOOT + 400, NOW)
        .unwrap();
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 200,
            NOW,
        )
        .unwrap();
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    assert_eq!(checkpoint.through_filetime, BOOT + 400);
    assert_eq!(checkpoint.processes.observed(10, BOOT + 350), None);
    assert_eq!(
        checkpoint.processes.observed(10, BOOT + 150).unwrap().0,
        identity(10, 100, Some(11))
    );
}

#[test]
fn late_start_merges_the_previously_persisted_stop() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![Lifecycle::End {
                pid: 10,
                start: BOOT + 100,
                end: BOOT + 200,
                key: Some(11),
            }],
            BOOT + 400,
            NOW,
        )
        .unwrap();
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 150,
            NOW,
        )
        .unwrap();
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    assert_eq!(checkpoint.processes.observed(10, BOOT + 350), None);
    assert_eq!(
        checkpoint.processes.observed(10, BOOT + 150).unwrap().0,
        identity(10, 100, Some(11))
    );
}

#[test]
fn late_parent_start_completes_the_captured_child_relation() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(20, 200, Some(22), 10)],
            BOOT + 400,
            NOW,
        )
        .unwrap();
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 150,
            NOW,
        )
        .unwrap();
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    assert_eq!(
        checkpoint
            .processes
            .ancestors(&identity(20, 200, Some(22)), 0),
        [identity(10, 100, Some(11))]
    );
}

#[test]
fn enriched_parent_identity_survives_retirement_and_equal_time_reuse() {
    let mut processes = crate::ancestry::Processes::default();
    processes.apply(start(10, 100, None, 0));
    processes.apply(start(20, 200, Some(33), 10));
    processes.apply(Lifecycle::Context {
        pid: 10,
        key: 11,
        time: BOOT + 250,
    });
    let mut processes: crate::ancestry::Processes =
        serde_json::from_slice(&serde_json::to_vec(&processes).unwrap()).unwrap();
    assert!(processes.evict_oldest());
    processes.apply(start(10, 100, Some(22), 0));
    assert_eq!(
        processes.observed(10, BOOT + 250).unwrap().0,
        identity(10, 100, Some(22))
    );
    assert_eq!(
        processes.ancestors(&identity(20, 200, Some(33)), 0),
        [identity(10, 100, Some(11))]
    );
}

#[test]
fn stopped_capture_boundary_survives_reopen_replay_and_resume() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 200,
            NOW,
        )
        .unwrap();
    journal.process_capture(BOOT, 0, None).unwrap();
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0), start(20, 300, Some(22), 10)],
            BOOT + 400,
            NOW,
        )
        .unwrap();
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    assert_eq!(checkpoint.processes.observed(10, BOOT + 350), None);
    assert_eq!(checkpoint.processes.observed(20, BOOT + 350), None);
    assert_eq!(
        checkpoint.processes.observed(10, BOOT + 150).unwrap().0,
        identity(10, 100, Some(11))
    );
    journal.process_capture(BOOT, 0, Some(BOOT + 500)).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 600, Some(33), 0)],
            BOOT + 800,
            NOW,
        )
        .unwrap();
    drop(journal);
    let journal = dir.open(RetentionPolicy::default());
    assert_eq!(
        journal
            .process_checkpoint(BOOT)
            .unwrap()
            .processes
            .observed(20, BOOT + 700),
        None
    );
    assert_eq!(
        journal
            .process_checkpoint(BOOT)
            .unwrap()
            .processes
            .observed(10, BOOT + 700)
            .unwrap()
            .0,
        identity(10, 600, Some(33))
    );
}

#[test]
fn capture_resume_write_failure_keeps_the_open_interruption() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 200,
            NOW,
        )
        .unwrap();
    journal.process_capture(BOOT, 0, None).unwrap();
    fs::create_dir(dir.0.join("metadata.pending")).unwrap();
    assert!(journal.process_capture(BOOT, 0, Some(BOOT + 300)).is_err());
    drop(journal);
    fs::remove_dir(dir.0.join("metadata.pending")).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .checkpoint_processes(
            &source(),
            vec![start(20, 400, Some(22), 0)],
            BOOT + 600,
            NOW,
        )
        .unwrap();
    assert_eq!(
        journal
            .process_checkpoint(BOOT)
            .unwrap()
            .processes
            .observed(20, BOOT + 500),
        None
    );
    journal.process_capture(BOOT, 0, Some(BOOT + 700)).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(20, 800, Some(33), 0)],
            BOOT + 1000,
            NOW,
        )
        .unwrap();
    assert_eq!(
        journal
            .process_checkpoint(BOOT)
            .unwrap()
            .processes
            .observed(20, BOOT + 900)
            .unwrap()
            .0,
        identity(20, 800, Some(33))
    );
}

#[test]
fn replacement_capture_start_restores_its_recorded_interval_after_reopen() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 200,
            NOW,
        )
        .unwrap();
    journal.process_capture(BOOT, BOOT + 200, None).unwrap();
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 500, Some(22), 0), start(20, 600, Some(33), 10)],
            BOOT + 800,
            NOW,
        )
        .unwrap();
    assert_eq!(
        journal
            .process_checkpoint(BOOT)
            .unwrap()
            .processes
            .observed(10, BOOT + 700),
        None
    );
    journal.process_capture(BOOT, 0, Some(BOOT + 400)).unwrap();
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    assert_eq!(checkpoint.processes.observed(10, BOOT + 350), None);
    assert_eq!(
        checkpoint.processes.observed(10, BOOT + 700).unwrap().0,
        identity(10, 500, Some(22))
    );
    assert_eq!(
        checkpoint
            .processes
            .ancestors(&identity(20, 600, Some(33)), 0),
        [identity(10, 500, Some(22))]
    );
}

#[test]
fn interruption_retirement_preserves_recent_intervals_and_bounds_replay() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy {
        max_bytes: 16 * 1024,
        ..Default::default()
    });
    journal.discover_imports(vec![source()]).unwrap();
    for index in 0..100 {
        let from = BOOT + index * 100 + 200;
        journal
            .checkpoint_processes(
                &source(),
                vec![Lifecycle::Gap {
                    from,
                    through: from + 20,
                }],
                from + 20,
                NOW,
            )
            .unwrap();
    }
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0), start(20, 10200, Some(22), 0)],
            BOOT + 10400,
            NOW,
        )
        .unwrap();
    drop(journal);
    let journal = dir.open(RetentionPolicy {
        max_bytes: 16 * 1024,
        ..Default::default()
    });
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    assert_eq!(checkpoint.processes.observed(10, BOOT + 10300), None);
    assert_eq!(
        checkpoint.processes.observed(20, BOOT + 10300).unwrap().0,
        identity(20, 10200, Some(22))
    );
    assert!(journal.measure_disk_bytes().unwrap() <= 16 * 1024);
}

#[test]
fn captured_parent_key_resolves_when_the_parent_start_arrives_later() {
    let mut processes = crate::ancestry::Processes::default();
    processes.apply(Lifecycle::Start {
        identity: identity(20, 200, Some(22)),
        parent: 10,
        parent_key: Some(11),
        session: Some(1),
    });
    assert_eq!(processes.ancestors(&identity(20, 200, Some(22)), 0), []);
    processes.apply(start(10, 100, None, 0));
    assert_eq!(
        processes.ancestors(&identity(20, 200, Some(22)), 0),
        [identity(10, 100, Some(11))]
    );
}

#[test]
fn keyless_bound_parent_keeps_its_captured_identity_after_replacement() {
    let mut processes = crate::ancestry::Processes::default();
    processes.apply(start(10, 100, None, 0));
    processes.apply(start(20, 200, Some(33), 10));
    assert!(processes.evict_oldest());
    processes.apply(start(10, 100, Some(22), 0));
    assert_eq!(
        processes.observed(10, BOOT + 250).unwrap().0,
        identity(10, 100, Some(22))
    );
    assert_eq!(
        processes.ancestors(&identity(20, 200, Some(33)), 0),
        [identity(10, 100, None)]
    );
}

#[test]
fn retired_keyless_parent_stays_captured_when_a_replacement_later_gains_its_key() {
    let mut processes = crate::ancestry::Processes::default();
    processes.apply(start(10, 100, None, 0));
    processes.apply(start(20, 200, Some(33), 10));
    assert!(processes.evict_oldest());
    let mut processes: crate::ancestry::Processes =
        serde_json::from_slice(&serde_json::to_vec(&processes).unwrap()).unwrap();
    processes.apply(start(10, 100, None, 0));
    processes.apply(Lifecycle::Context {
        pid: 10,
        key: 22,
        time: BOOT + 250,
    });
    assert_eq!(
        processes.observed(10, BOOT + 250).unwrap().0,
        identity(10, 100, Some(22))
    );
    assert_eq!(
        processes.ancestors(&identity(20, 200, Some(33)), 0),
        [identity(10, 100, None)]
    );
}

#[test]
fn prior_checkpoint_array_restores_captured_identity() {
    let value = json!([{"identity":identity(10, 100, Some(11)), "parent":null,"session":1,"ended":BOOT+200}]);
    let processes: crate::ancestry::Processes = serde_json::from_value(value).unwrap();
    assert_eq!(
        processes.observed(10, BOOT + 150).unwrap().0,
        identity(10, 100, Some(11))
    );
    assert_eq!(processes.observed(10, BOOT + 250), None);
}

#[test]
fn repeated_start_enriches_bound_parents_before_retirement() {
    let mut processes = crate::ancestry::Processes::default();
    processes.apply(start(10, 100, None, 0));
    processes.apply(start(20, 200, Some(33), 10));
    processes.apply(start(10, 100, Some(11), 0));
    assert!(processes.evict_oldest());
    processes.apply(start(10, 100, Some(22), 0));
    assert_eq!(
        processes.observed(10, BOOT + 250).unwrap().0,
        identity(10, 100, Some(22))
    );
    assert_eq!(
        processes.ancestors(&identity(20, 200, Some(33)), 0),
        [identity(10, 100, Some(11))]
    );
}

#[test]
fn late_parent_across_an_interruption_keeps_the_child_unresolved() {
    let mut processes = crate::ancestry::Processes::default();
    processes.apply(Lifecycle::Gap {
        from: BOOT + 150,
        through: BOOT + 250,
    });
    processes.apply(start(20, 300, Some(33), 10));
    processes.apply(start(10, 100, Some(11), 0));
    assert_eq!(
        processes.observed(20, BOOT + 350).unwrap().0,
        identity(20, 300, Some(33))
    );
    assert_eq!(processes.ancestors(&identity(20, 300, Some(33)), 0), []);
}

#[test]
fn late_stop_and_reused_parent_start_merge_at_the_child_creation_time() {
    let mut processes = crate::ancestry::Processes::default();
    processes.apply(start(10, 100, Some(11), 0));
    processes.apply(start(20, 200, Some(33), 10));
    processes.apply(Lifecycle::End {
        pid: 10,
        start: BOOT + 100,
        end: BOOT + 150,
        key: Some(11),
    });
    assert_eq!(processes.ancestors(&identity(20, 200, Some(33)), 0), []);
    processes.apply(start(10, 180, Some(22), 0));
    assert_eq!(
        processes.ancestors(&identity(20, 200, Some(33)), 0),
        [identity(10, 180, Some(22))]
    );
}

fn identity(pid: u32, offset: u64, key: Option<u64>) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        start_time_filetime: Some((BOOT + offset).to_string()),
        boot_time_filetime: Some(BOOT.to_string()),
        start_key: key.map(|key| key.to_string()),
        exe_path: Some(format!("C:\\Processes\\{pid}.exe")),
    }
}

fn source() -> ImportSource {
    ImportSource {
        id: "process-file".into(),
        boot_filetime: BOOT,
    }
}

fn start(pid: u32, offset: u64, key: Option<u64>, parent: u32) -> Lifecycle {
    Lifecycle::Start {
        identity: identity(pid, offset, key),
        parent,
        parent_key: None,
        session: Some(1),
    }
}

fn record(pid: u32, key: Option<u64>) -> venera_debug_protocol::LogRecord {
    serde_json::from_value(json!({
        "stream_id":"offline", "seq":"1", "observed_at":"2026-11-17T07:30:00Z",
        "source":"etw", "component":"d3d12", "level":"info", "raw_level":"info", "text":"offline child log",
        "process":{"pid":pid,"start_time_filetime":null,"exe_path":null,
            "boot_time_filetime":BOOT.to_string(),"start_key":key.map(|key| key.to_string())}
    })).unwrap()
}

#[test]
fn offline_parent_child_and_pid_reuse_survive_checkpoint_and_import_restarts() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![
                start(10, 100, Some(11), 0),
                start(20, 200, Some(22), 10),
                Lifecycle::End {
                    pid: 10,
                    start: BOOT + 100,
                    end: BOOT + 300,
                    key: Some(11),
                },
                Lifecycle::End {
                    pid: 20,
                    start: BOOT + 200,
                    end: BOOT + 400,
                    key: Some(22),
                },
                start(10, 500, Some(33), 0),
            ],
            BOOT + 600,
            NOW,
        )
        .unwrap();
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    let mut child = record(20, Some(22));
    assert!(checkpoint.attribute_record(&mut child, BOOT, BOOT + 350, true));
    assert_eq!(child.process, Some(identity(20, 200, Some(22))));
    assert_eq!(child.ancestors, [identity(10, 100, Some(11))]);
    let filter = venera_debug_protocol::Filter {
        targets: vec![venera_debug_protocol::Target::ProcessInstance {
            pid: 10,
            start_time_filetime: (BOOT + 100).to_string(),
            include_descendants: true,
            boot_time_filetime: Some(BOOT.to_string()),
            start_key: Some("11".into()),
        }],
        ..Default::default()
    };
    assert!(venera_debug_protocol::matches_filter(
        &filter,
        &child,
        &child.ancestors
    ));
    let mut other_boot = child.clone();
    other_boot.ancestors[0].boot_time_filetime = Some((BOOT - 1).to_string());
    assert!(!venera_debug_protocol::matches_filter(
        &filter,
        &other_boot,
        &other_boot.ancestors
    ));
    let mut other_key = child.clone();
    other_key.ancestors[0].start_key = Some("99".into());
    assert!(!venera_debug_protocol::matches_filter(
        &filter,
        &other_key,
        &other_key.ancestors
    ));
    let metadata = fs::read(dir.0.join("metadata.json")).unwrap();
    journal
        .import_batch(ImportBatch {
            source: source(),
            through: 8,
            complete: false,
            now_ms: NOW,
            entries: vec![ImportEntry {
                ordinal: 8,
                source_filetime: BOOT + 350,
                event: HistoryEvent::Log {
                    record: child.clone(),
                },
            }],
        })
        .unwrap();
    drop(journal);
    fs::write(dir.0.join("metadata.json"), metadata).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .import_batch(ImportBatch {
            source: source(),
            through: 8,
            complete: true,
            now_ms: NOW,
            entries: vec![ImportEntry {
                ordinal: 8,
                source_filetime: BOOT + 350,
                event: HistoryEvent::Log { record: child },
            }],
        })
        .unwrap();
    let records = journal
        .read(
            HistoryRead {
                after: None,
                through: None,
                limit: 10,
            },
            100_000,
        )
        .unwrap()
        .records;
    assert_eq!(records.len(), 1);
    let HistoryEvent::Log { record: child } = &records[0].event else {
        panic!("child log")
    };
    assert_eq!(child.ancestors, [identity(10, 100, Some(11))]);
    let mut reused = record(10, Some(33));
    checkpoint.attribute_record(&mut reused, BOOT, BOOT + 550, true);
    assert_eq!(reused.process, Some(identity(10, 500, Some(33))));
    assert!(!venera_debug_protocol::matches_filter(
        &filter,
        &reused,
        &reused.ancestors
    ));
}

#[test]
fn boot_key_and_capture_interval_prevent_historical_pid_aliases() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![
                start(10, 100, Some(11), 0),
                start(10, 500, Some(22), 0),
                Lifecycle::End {
                    pid: 10,
                    start: BOOT + 500,
                    end: BOOT + 600,
                    key: Some(22),
                },
            ],
            BOOT + 1000,
            NOW,
        )
        .unwrap();
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    assert_eq!(
        checkpoint
            .processes
            .attribute(&record(10, Some(22)).process.unwrap(), BOOT, BOOT + 200),
        None
    );
    assert_eq!(
        checkpoint
            .processes
            .attribute(&record(10, None).process.unwrap(), BOOT, BOOT + 700),
        None
    );
    assert_eq!(
        checkpoint.processes.attribute(
            &record(10, Some(11)).process.unwrap(),
            BOOT + 1,
            BOOT + 200
        ),
        None
    );
    assert_eq!(
        checkpoint
            .processes
            .attribute(&record(10, Some(11)).process.unwrap(), BOOT, BOOT + 200)
            .unwrap()
            .0,
        identity(10, 100, Some(11))
    );
    assert!(journal
        .process_checkpoint(BOOT + 1)
        .unwrap()
        .processes
        .is_empty());
}

#[test]
fn source_creation_time_supports_older_events_and_kernel_context_stays_unattributed() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(&source(), vec![start(20, 200, None, 0)], BOOT + 600, NOW)
        .unwrap();
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    let mut old = record(20, None);
    assert!(checkpoint.attribute_record(&mut old, BOOT, BOOT + 300, true));
    assert_eq!(old.process, Some(identity(20, 200, None)));
    let mut pending = record(20, None);
    assert!(!checkpoint.attribute_record(&mut pending, BOOT, BOOT + 700, true));
    for transport in ["etw", "wpp_recorder"] {
        let mut kernel = record(20, None);
        kernel.component = "kernel".into();
        kernel.source = transport.into();
        assert!(checkpoint.attribute_record(&mut kernel, BOOT, BOOT + 300, true));
        assert_eq!(kernel.process, None);
        assert!(kernel.ancestors.is_empty());
    }
}

#[test]
fn checkpoint_replacement_failure_preserves_previous_context_and_source_progress() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 200,
            NOW,
        )
        .unwrap();
    fs::create_dir(dir.0.join("metadata.pending")).unwrap();
    assert!(journal
        .checkpoint_processes(
            &source(),
            vec![start(20, 300, Some(22), 10)],
            BOOT + 400,
            NOW
        )
        .is_err());
    drop(journal);
    fs::remove_dir(dir.0.join("metadata.pending")).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    assert_eq!(journal.import_receipt(&source()).unwrap().through, 0);
    assert_eq!(
        journal.process_checkpoint(BOOT).unwrap().through_filetime,
        BOOT + 200
    );
    journal
        .checkpoint_processes(
            &source(),
            vec![start(20, 300, Some(22), 10)],
            BOOT + 400,
            NOW,
        )
        .unwrap();
    let mut child = record(20, Some(22));
    journal
        .process_checkpoint(BOOT)
        .unwrap()
        .attribute_record(&mut child, BOOT, BOOT + 350, true);
    assert_eq!(child.ancestors, [identity(10, 100, Some(11))]);
}

#[test]
fn process_checkpoints_share_the_physical_journal_budget() {
    let dir = Directory::new();
    let policy = RetentionPolicy {
        max_bytes: 16 * 1024,
        ..Default::default()
    };
    let mut journal = dir.open(policy.clone());
    journal.discover_imports(vec![source()]).unwrap();
    let retired = journal
        .checkpoint_processes(
            &source(),
            (1..=128)
                .map(|pid| start(pid, pid as u64, Some(pid as u64), 0))
                .collect(),
            BOOT + 500,
            NOW,
        )
        .unwrap();
    assert!(retired > 0);
    assert_eq!(
        journal
            .import_receipt(&source())
            .unwrap()
            .process_context_lost,
        retired
    );
    assert!(journal.measure_disk_bytes().unwrap() <= 16 * 1024);
    drop(journal);
    let journal = dir.open(policy);
    assert_eq!(
        journal
            .process_checkpoint(BOOT)
            .unwrap()
            .processes
            .observed(128, BOOT + 200)
            .unwrap()
            .0,
        identity(128, 128, Some(128))
    );
    assert!(journal.measure_disk_bytes().unwrap() <= 16 * 1024);
}

#[test]
fn extended_context_keys_enrich_exact_instances_and_keep_bound_parents() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![
                start(10, 100, None, 0),
                Lifecycle::Context {
                    pid: 10,
                    key: 11,
                    time: BOOT + 150,
                },
                start(20, 200, None, 10),
                Lifecycle::Context {
                    pid: 20,
                    key: 22,
                    time: BOOT + 250,
                },
                start(10, 300, Some(33), 0),
            ],
            BOOT + 500,
            NOW,
        )
        .unwrap();
    drop(journal);
    let journal = dir.open(RetentionPolicy::default());
    let mut child = record(20, Some(22));
    journal
        .process_checkpoint(BOOT)
        .unwrap()
        .attribute_record(&mut child, BOOT, BOOT + 400, true);
    assert_eq!(child.process, Some(identity(20, 200, Some(22))));
    assert_eq!(child.ancestors, [identity(10, 100, Some(11))]);
}

#[test]
fn process_gaps_retire_pid_assumptions_before_a_later_trace() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 100, Some(11), 0)],
            BOOT + 200,
            NOW,
        )
        .unwrap();
    journal.reconcile_imports(&[], BOOT, NOW).unwrap();
    assert_eq!(
        journal
            .process_checkpoint(BOOT)
            .unwrap()
            .processes
            .observed(10, BOOT + 300),
        None
    );
    assert_eq!(
        journal
            .process_checkpoint(BOOT)
            .unwrap()
            .processes
            .observed(10, BOOT + 150)
            .unwrap()
            .0,
        identity(10, 100, Some(11))
    );
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![start(10, 500, Some(22), 0)],
            BOOT + 600,
            NOW,
        )
        .unwrap();
    let mut later = record(10, Some(22));
    journal
        .process_checkpoint(BOOT)
        .unwrap()
        .attribute_record(&mut later, BOOT, BOOT + 550, true);
    assert_eq!(later.process, Some(identity(10, 500, Some(22))));
    assert_eq!(journal.range().unwrap().high_water.0, 1);
}

#[test]
fn start_keys_distinguish_instances_even_with_equal_pid_and_creation_time() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source()]).unwrap();
    journal
        .checkpoint_processes(
            &source(),
            vec![
                start(10, 100, Some(11), 0),
                Lifecycle::End {
                    pid: 10,
                    start: BOOT + 100,
                    end: BOOT + 200,
                    key: Some(11),
                },
                start(10, 100, Some(22), 0),
                Lifecycle::Start {
                    identity: identity(20, 300, Some(33)),
                    parent: 10,
                    parent_key: Some(22),
                    session: Some(1),
                },
            ],
            BOOT + 500,
            NOW,
        )
        .unwrap();
    drop(journal);
    let journal = dir.open(RetentionPolicy::default());
    let checkpoint = journal.process_checkpoint(BOOT).unwrap();
    let mut first = record(10, Some(11));
    checkpoint.attribute_record(&mut first, BOOT, BOOT + 150, true);
    assert_eq!(first.process, Some(identity(10, 100, Some(11))));
    let mut second = record(10, Some(22));
    checkpoint.attribute_record(&mut second, BOOT, BOOT + 250, true);
    assert_eq!(second.process, Some(identity(10, 100, Some(22))));
    assert_eq!(
        checkpoint
            .processes
            .attribute(&record(10, None).process.unwrap(), BOOT, BOOT + 150),
        None
    );
    let mut child = record(20, Some(33));
    checkpoint.attribute_record(&mut child, BOOT, BOOT + 350, true);
    assert_eq!(child.ancestors, [identity(10, 100, Some(22))]);
}
