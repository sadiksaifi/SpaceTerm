use super::*;
use crate::platform::{
    app_directories::AppDirectoryEnvironment,
    testing::{RecordingFilesystem, resolve_app_paths},
};

fn storage() -> (Arc<AppPaths>, Arc<RecordingFilesystem>) {
    let filesystem = Arc::new(RecordingFilesystem::default());
    let paths = resolve_app_paths(
        &AppDirectoryEnvironment {
            home: Some("/home/test".into()),
            ..Default::default()
        },
        Some("/runtime".into()),
        200,
        filesystem.clone(),
    )
    .unwrap();
    (Arc::new(paths), filesystem)
}

fn facts(at: u64, status: ObservedCoreStatus) -> LastKnownFacts {
    LastKnownFacts {
        observed_at: at,
        status,
        machine: Some("Penguin".into()),
        operating_system: Some("Linux".into()),
        version: Some("0.1.0".into()),
        ..Default::default()
    }
}

fn network(at: u64) -> NetworkAccessFacts {
    let address = "192.0.2.10:4433".parse::<SocketAddr>().unwrap();
    NetworkAccessFacts {
        observed_at: at,
        machine_name: "Penguin".into(),
        policy: Some(address),
        listener: NetworkListenerFacts::Listening(address),
        addresses: Some(NetworkAddresses {
            available: vec![address.ip()],
            suggested: Some(address.ip()),
        }),
    }
}

fn device(index: u16) -> PairedDeviceFacts {
    let mut pairing = [1; 32];
    pairing[..2].copy_from_slice(&index.to_be_bytes());
    PairedDeviceFacts {
        pairing,
        name: "Studio".into(),
        fingerprint: ["0123ABCD"; 8].join(" "),
        last_connected_at: Some(10),
    }
}

#[test]
fn network_and_device_display_facts_survive_relaunch_independently() {
    let (paths, _) = storage();
    let key = MachineKey::Direct([1; 32]);
    let expected = LastKnownFacts {
        network_access: Some(network(30)),
        paired_devices: Some(PairedDevicesFacts {
            observed_at: 40,
            devices: vec![device(1)],
        }),
        ..facts(20, ObservedCoreStatus::Running)
    };
    let mut context = CoreContext::load(paths.clone()).unwrap();
    context.remember(key.clone(), expected.clone()).unwrap();
    let restored = CoreContext::load(paths).unwrap();
    assert_eq!(restored.facts(&key), Some(&expected));
}

#[test]
fn a_newer_general_observation_cannot_erase_known_network_or_devices() {
    let (paths, _) = storage();
    let key = MachineKey::Direct([1; 32]);
    let initial = LastKnownFacts {
        network_access: Some(network(30)),
        paired_devices: Some(PairedDevicesFacts {
            observed_at: 40,
            devices: vec![device(1)],
        }),
        ..facts(20, ObservedCoreStatus::Running)
    };
    let mut writer = CoreContext::load(paths.clone()).unwrap();
    let mut stale_window = CoreContext::load(paths.clone()).unwrap();
    writer.remember(key.clone(), initial.clone()).unwrap();
    stale_window
        .remember(key.clone(), facts(50, ObservedCoreStatus::Unavailable))
        .unwrap();
    let restored = CoreContext::load(paths).unwrap();
    let actual = restored.facts(&key).unwrap();
    assert_eq!(actual.status, ObservedCoreStatus::Unavailable);
    assert_eq!(actual.network_access, initial.network_access);
    assert_eq!(actual.paired_devices, initial.paired_devices);
}

#[test]
fn older_general_facts_can_carry_newer_network_and_device_observations() {
    let (paths, _) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut first = CoreContext::load(paths.clone()).unwrap();
    let mut stale_window = CoreContext::load(paths.clone()).unwrap();
    first
        .remember(
            key.clone(),
            LastKnownFacts {
                network_access: Some(network(30)),
                paired_devices: Some(PairedDevicesFacts {
                    observed_at: 40,
                    devices: vec![device(1)],
                }),
                ..facts(100, ObservedCoreStatus::Running)
            },
        )
        .unwrap();
    let incoming = LastKnownFacts {
        network_access: Some(NetworkAccessFacts {
            observed_at: 60,
            policy: None,
            listener: NetworkListenerFacts::Off,
            ..network(30)
        }),
        paired_devices: Some(PairedDevicesFacts {
            observed_at: 70,
            devices: Vec::new(),
        }),
        ..facts(50, ObservedCoreStatus::Stopped)
    };
    stale_window
        .remember(key.clone(), incoming.clone())
        .unwrap();
    let restored = CoreContext::load(paths).unwrap();
    let actual = restored.facts(&key).unwrap();
    assert_eq!(
        (actual.observed_at, actual.status),
        (100, ObservedCoreStatus::Running)
    );
    assert_eq!(actual.network_access, incoming.network_access);
    assert_eq!(actual.paired_devices, incoming.paired_devices);
}

#[test]
fn successful_state_and_device_reads_keep_their_own_latest_timestamps() {
    let (paths, _) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut first = CoreContext::load(paths.clone()).unwrap();
    let mut second = CoreContext::load(paths.clone()).unwrap();
    let initial = LastKnownFacts {
        network_access: Some(network(90)),
        paired_devices: Some(PairedDevicesFacts {
            observed_at: 40,
            devices: vec![device(1)],
        }),
        ..facts(100, ObservedCoreStatus::Running)
    };
    first.remember(key.clone(), initial.clone()).unwrap();
    let incoming = LastKnownFacts {
        network_access: Some(NetworkAccessFacts {
            observed_at: 80,
            policy: None,
            listener: NetworkListenerFacts::Off,
            ..network(30)
        }),
        paired_devices: Some(PairedDevicesFacts {
            observed_at: 70,
            devices: Vec::new(),
        }),
        ..facts(110, ObservedCoreStatus::Unavailable)
    };
    second.remember(key.clone(), incoming.clone()).unwrap();
    let restored = CoreContext::load(paths).unwrap();
    let actual = restored.facts(&key).unwrap();
    assert_eq!(actual.network_access, initial.network_access);
    assert_eq!(actual.paired_devices, incoming.paired_devices);
    assert_eq!(actual.observed_at, 110);
}

#[test]
fn malformed_network_observations_preserve_memory_and_exact_retained_bytes() {
    let (paths, filesystem) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut context = CoreContext::load(paths).unwrap();
    let initial = LastKnownFacts {
        network_access: Some(network(30)),
        ..facts(20, ObservedCoreStatus::Running)
    };
    context.remember(key.clone(), initial.clone()).unwrap();
    let before = filesystem.files.lock().unwrap().values.clone();
    let address = "192.0.2.10:4433".parse::<SocketAddr>().unwrap();
    let invalid = [
        NetworkAccessFacts {
            machine_name: "".into(),
            ..network(40)
        },
        NetworkAccessFacts {
            machine_name: "x".repeat(513),
            ..network(40)
        },
        NetworkAccessFacts {
            machine_name: "\u{202e}Penguin".into(),
            ..network(40)
        },
        NetworkAccessFacts {
            policy: Some("0.0.0.0:4433".parse().unwrap()),
            ..network(40)
        },
        NetworkAccessFacts {
            policy: Some("192.0.2.10:0".parse().unwrap()),
            ..network(40)
        },
        NetworkAccessFacts {
            policy: None,
            ..network(40)
        },
        NetworkAccessFacts {
            listener: NetworkListenerFacts::Listening("192.0.2.11:4433".parse().unwrap()),
            ..network(40)
        },
        NetworkAccessFacts {
            addresses: Some(NetworkAddresses {
                available: vec![address.ip(), address.ip()],
                suggested: None,
            }),
            ..network(40)
        },
        NetworkAccessFacts {
            addresses: Some(NetworkAddresses {
                available: vec![address.ip()],
                suggested: Some("192.0.2.11".parse().unwrap()),
            }),
            ..network(40)
        },
    ];
    for observation in invalid {
        assert_eq!(
            context.remember(
                key.clone(),
                LastKnownFacts {
                    network_access: Some(observation),
                    ..facts(50, ObservedCoreStatus::Running)
                }
            ),
            Err(ContextError::Malformed)
        );
        assert_eq!(context.facts(&key), Some(&initial));
        assert_eq!(filesystem.files.lock().unwrap().values, before);
    }
}

#[test]
fn malformed_device_observations_preserve_memory_and_exact_retained_bytes() {
    let (paths, filesystem) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut context = CoreContext::load(paths).unwrap();
    let initial = LastKnownFacts {
        paired_devices: Some(PairedDevicesFacts {
            observed_at: 30,
            devices: vec![device(1)],
        }),
        ..facts(20, ObservedCoreStatus::Running)
    };
    context.remember(key.clone(), initial.clone()).unwrap();
    let before = filesystem.files.lock().unwrap().values.clone();
    for devices in [
        vec![PairedDeviceFacts {
            pairing: [0; 32],
            ..device(1)
        }],
        vec![device(1), device(1)],
        (0..257).map(device).collect(),
        vec![PairedDeviceFacts {
            name: "x".repeat(49),
            ..device(1)
        }],
        vec![PairedDeviceFacts {
            name: "\nStudio".into(),
            ..device(1)
        }],
        vec![PairedDeviceFacts {
            fingerprint: "not a fingerprint".into(),
            ..device(1)
        }],
        vec![PairedDeviceFacts {
            last_connected_at: Some(253_402_300_800),
            ..device(1)
        }],
    ] {
        assert_eq!(
            context.remember(
                key.clone(),
                LastKnownFacts {
                    paired_devices: Some(PairedDevicesFacts {
                        observed_at: 40,
                        devices
                    }),
                    ..facts(50, ObservedCoreStatus::Running)
                }
            ),
            Err(ContextError::Malformed)
        );
        assert_eq!(context.facts(&key), Some(&initial));
        assert_eq!(filesystem.files.lock().unwrap().values, before);
    }
}

#[test]
fn oversized_device_facts_reject_the_whole_candidate_before_changing_memory_or_disk() {
    let (paths, filesystem) = storage();
    let mut context = CoreContext::load(paths.clone()).unwrap();
    let mut stale_window = CoreContext::load(paths).unwrap();
    let devices: Vec<_> = (0..256)
        .map(|index| PairedDeviceFacts {
            name: "\u{10400}".repeat(48),
            ..device(index)
        })
        .collect();
    for index in 0..10 {
        let key = MachineKey::Direct([index + 1; 32]);
        let before = filesystem.files.lock().unwrap().values.clone();
        let previous: Vec<_> = (0..index)
            .map(|previous| {
                let key = MachineKey::Direct([previous + 1; 32]);
                let facts = context.facts(&key).cloned();
                (key, facts)
            })
            .collect();
        let incoming = LastKnownFacts {
            paired_devices: Some(PairedDevicesFacts {
                observed_at: 30,
                devices: devices.clone(),
            }),
            ..facts(20, ObservedCoreStatus::Running)
        };
        match context.remember(key.clone(), incoming.clone()) {
            Ok(()) => {}
            Err(ContextError::Malformed) => {
                assert!(context.facts(&key).is_none());
                assert_eq!(filesystem.files.lock().unwrap().values, before);
                for (key, expected) in previous {
                    assert_eq!(context.facts(&key), expected.as_ref());
                }
                // This candidate fits the stale window alone, but not the authoritative file.
                assert_eq!(
                    stale_window.remember(key.clone(), incoming),
                    Err(ContextError::Malformed)
                );
                assert!(stale_window.facts(&key).is_none());
                assert_eq!(filesystem.files.lock().unwrap().values, before);
                return;
            }
            Err(other) => panic!("unexpected retention error: {other:?}"),
        }
    }
    panic!("ten full 256-device observations must exceed the 512 KiB context bound");
}

#[test]
fn storage_unavailability_applies_only_the_validated_merged_display_facts_in_memory() {
    let (paths, filesystem) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut context = CoreContext::load(paths.clone()).unwrap();
    let initial = LastKnownFacts {
        network_access: Some(network(30)),
        paired_devices: Some(PairedDevicesFacts {
            observed_at: 40,
            devices: vec![device(1)],
        }),
        ..facts(100, ObservedCoreStatus::Running)
    };
    context.remember(key.clone(), initial.clone()).unwrap();
    let before = filesystem.files.lock().unwrap().values.clone();
    filesystem.files.lock().unwrap().read_failure = Some(SecureFilesystemError::Unavailable);
    let incoming = LastKnownFacts {
        network_access: Some(NetworkAccessFacts {
            observed_at: 60,
            policy: None,
            listener: NetworkListenerFacts::Off,
            ..network(30)
        }),
        ..Default::default()
    };
    assert_eq!(
        context.remember(key.clone(), incoming.clone()),
        Err(ContextError::Unavailable)
    );
    let actual = context.facts(&key).unwrap();
    assert_eq!(actual.status, ObservedCoreStatus::Running);
    assert_eq!(actual.network_access, incoming.network_access);
    assert_eq!(actual.paired_devices, initial.paired_devices);
    assert_eq!(filesystem.files.lock().unwrap().values, before);
    filesystem.files.lock().unwrap().read_failure = None;
    assert_eq!(
        CoreContext::load(paths).unwrap().facts(&key),
        Some(&initial)
    );
}

#[test]
fn malformed_network_records_are_preserved_during_load_and_later_updates() {
    let (paths, filesystem) = storage();
    let key = MachineKey::Direct([1; 32]);
    let initial = LastKnownFacts {
        network_access: Some(network(30)),
        ..facts(20, ObservedCoreStatus::Running)
    };
    let mut context = CoreContext::load(paths.clone()).unwrap();
    context.remember(key.clone(), initial.clone()).unwrap();
    {
        let mut files = filesystem.files.lock().unwrap();
        let bytes = &mut files.values.values_mut().next().unwrap().0;
        let mut document: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        document["facts"][0]["facts"]["network_access"]["machine_name"] = "\u{202e}hidden".into();
        *bytes = serde_json::to_vec(&document).unwrap();
    }
    let before = filesystem.files.lock().unwrap().values.clone();
    assert!(matches!(
        CoreContext::load(paths),
        Err(ContextError::Malformed)
    ));
    assert_eq!(
        context.remember(key.clone(), facts(50, ObservedCoreStatus::Unavailable)),
        Err(ContextError::Malformed)
    );
    assert_eq!(context.facts(&key), Some(&initial));
    assert_eq!(filesystem.files.lock().unwrap().values, before);
}

#[test]
fn selection_and_last_known_facts_survive_relaunch_without_connection_authority() {
    let (paths, filesystem) = storage();
    let key = MachineKey::Direct([1; 32]);
    let expected = LastKnownFacts {
        worktree_location: Some("/owning-server-only/{repository}/{branch}".into()),
        ..facts(20, ObservedCoreStatus::Running)
    };
    {
        let mut context = CoreContext::load(paths.clone()).unwrap();
        context.select(key.clone()).unwrap();
        context.remember(key.clone(), expected.clone()).unwrap();
    }
    let context = CoreContext::load(paths).unwrap();
    assert_eq!(context.selected(), &key);
    assert_eq!(context.facts(&key), Some(&expected));
    let files = filesystem.files.lock().unwrap();
    assert_eq!(files.values.len(), 1);
    assert!(
        files
            .values
            .keys()
            .all(|path| path.file_name() == Some(OsStr::new(FILE)))
    );
}

#[test]
fn malformed_worktree_location_observations_preserve_the_prior_retained_record() {
    let (paths, filesystem) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut context = CoreContext::load(paths.clone()).unwrap();
    let expected = LastKnownFacts {
        worktree_location: Some("~/worktrees/{repository}/{branch}".into()),
        ..facts(20, ObservedCoreStatus::Running)
    };
    context.remember(key.clone(), expected.clone()).unwrap();
    let prior = filesystem.files.lock().unwrap().values.clone();
    let oversized = format!("/{}{{branch}}", "a".repeat(1024));
    for template in [
        "",
        "~/worktrees/{unknown}/{branch}",
        "~/worktrees/{repository}",
        "worktrees/{branch}",
        "~/../{branch}",
        "~/\n/{branch}",
        "~/\0/{branch}",
        &oversized,
    ] {
        let invalid = LastKnownFacts {
            observed_at: 21,
            worktree_location: Some(template.into()),
            ..expected.clone()
        };
        assert_eq!(
            context.remember(key.clone(), invalid),
            Err(ContextError::Malformed)
        );
        assert_eq!(context.facts(&key), Some(&expected));
    }
    assert_eq!(filesystem.files.lock().unwrap().values, prior);
    assert_eq!(
        CoreContext::load(paths).unwrap().facts(&key),
        Some(&expected)
    );
}

#[test]
fn removing_the_selected_machine_falls_back_to_local_and_forgets_its_facts() {
    let (paths, _) = storage();
    let key = MachineKey::Direct([2; 32]);
    let other = MachineKey::Direct([1; 32]);
    let mut context = CoreContext::load(paths.clone()).unwrap();
    context.select(key.clone()).unwrap();
    context
        .remember(key.clone(), facts(20, ObservedCoreStatus::Running))
        .unwrap();
    context
        .remember(other.clone(), facts(10, ObservedCoreStatus::Stopped))
        .unwrap();
    context.reconcile(std::slice::from_ref(&other)).unwrap();
    let restored = CoreContext::load(paths).unwrap();
    assert_eq!(restored.selected(), &MachineKey::Local);
    assert!(restored.facts(&key).is_none());
    assert_eq!(
        restored.facts(&other).unwrap().status,
        ObservedCoreStatus::Stopped
    );
}

#[test]
fn a_missing_remembered_machine_falls_back_after_the_saved_inventory_loads() {
    let (paths, _) = storage();
    let mut context = CoreContext::load(paths.clone()).unwrap();
    context.select(MachineKey::Direct([2; 32])).unwrap();
    let mut restored = CoreContext::load(paths.clone()).unwrap();
    restored.reconcile(&[]).unwrap();
    assert_eq!(restored.selected(), &MachineKey::Local);
    assert_eq!(
        CoreContext::load(paths).unwrap().selected(),
        &MachineKey::Local
    );
}

#[test]
fn delayed_observations_never_replace_newer_retained_facts() {
    let (paths, _) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut first = CoreContext::load(paths.clone()).unwrap();
    let mut delayed = CoreContext::load(paths.clone()).unwrap();
    let expected = facts(20, ObservedCoreStatus::Stopped);
    first.remember(key.clone(), expected.clone()).unwrap();
    delayed
        .remember(key.clone(), facts(10, ObservedCoreStatus::Running))
        .unwrap();
    assert_eq!(
        CoreContext::load(paths).unwrap().facts(&key),
        Some(&expected)
    );
}

#[test]
fn remembering_facts_does_not_overwrite_another_windows_selection() {
    let (paths, _) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut first = CoreContext::load(paths.clone()).unwrap();
    let mut second = CoreContext::load(paths.clone()).unwrap();
    first.select(key.clone()).unwrap();
    second
        .remember(MachineKey::Local, facts(20, ObservedCoreStatus::Running))
        .unwrap();
    assert_eq!(CoreContext::load(paths).unwrap().selected(), &key);
}

#[test]
fn failed_retention_still_removes_a_selected_machine_from_presentation() {
    let (paths, filesystem) = storage();
    let key = MachineKey::Direct([1; 32]);
    let mut context = CoreContext::load(paths).unwrap();
    context.select(key).unwrap();
    filesystem.files.lock().unwrap().prepare_error = Some(SecureFilesystemError::Unavailable);
    assert_eq!(context.reconcile(&[]), Err(ContextError::Unavailable));
    assert_eq!(context.selected(), &MachineKey::Local);
}

#[test]
fn malformed_storage_is_preserved_when_using_the_inert_local_fallback() {
    let (paths, filesystem) = storage();
    let mut context = CoreContext::load(paths.clone()).unwrap();
    context.select(MachineKey::Direct([1; 32])).unwrap();
    {
        let mut files = filesystem.files.lock().unwrap();
        files.values.values_mut().next().unwrap().0 = b"malformed".to_vec();
    }
    let mut fallback = CoreContext::new(paths);
    assert_eq!(
        fallback.select(MachineKey::Direct([3; 32])),
        Err(ContextError::Malformed)
    );
    let files = filesystem.files.lock().unwrap();
    assert_eq!(files.values.values().next().unwrap().0, b"malformed");
}
