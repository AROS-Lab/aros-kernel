//! Envelope property tests — parametrized round-trip and invariant checks
//! across `SecurityZone × Priority × ResourceBudget` combinations.
//!
//! These exist because `serde_json` round-tripping has historically been a source
//! of silent regressions when adding fields, and budget invariants (warning
//! threshold, exceeded check) must hold across the full input space — not just
//! the default budget that the existing single-instance tests in
//! `src/envelope/task_envelope.rs` exercise.

use std::collections::HashMap;
use std::time::Duration;

use aros_kernel::envelope::task_envelope::{
    CheckpointPolicy, ENVELOPE_VERSION, Priority, ResourceBudget, SecurityZone, TaskEnvelope,
    TaskSpec, ToolEndpoint,
};

const ALL_ZONES: [SecurityZone; 3] =
    [SecurityZone::Green, SecurityZone::Yellow, SecurityZone::Red];

const ALL_PRIORITIES: [Priority; 3] = [
    Priority::P0Critical,
    Priority::P1Normal,
    Priority::P2Background,
];

fn make_envelope(zone: SecurityZone, priority: Priority, budget: ResourceBudget) -> TaskEnvelope {
    let spec = TaskSpec {
        title: format!("task-{:?}-{:?}", zone, priority),
        description: "property test".into(),
        working_dir: Some("/tmp/work".into()),
        env_vars: HashMap::from([("K".into(), "V".into())]),
        max_retries: 2,
    };
    let mut env = TaskEnvelope::new("task-pp", "dag-pp", spec, zone, priority);
    env.resource_budget = budget;
    env.tool_endpoints.push(ToolEndpoint {
        name: "bash".into(),
        socket_path: "/tmp/bash.sock".into(),
        capabilities: vec!["execute".into()],
    });
    env.checkpoint_policy = CheckpointPolicy {
        interval: Duration::from_secs(45),
        on_tool_call: false,
    };
    env
}

/// Property: every (zone, priority) pair round-trips through JSON without losing
/// fields, preserves variant equality, and remains valid post-deserialization.
#[test]
fn envelope_round_trip_all_zone_priority_combinations() {
    for zone in ALL_ZONES {
        for priority in ALL_PRIORITIES {
            let original = make_envelope(zone, priority, ResourceBudget::default());
            assert!(
                original.validate().is_ok(),
                "default-budget envelope must validate for ({zone:?}, {priority:?})"
            );

            let json = serde_json::to_string(&original)
                .unwrap_or_else(|e| panic!("serialize ({zone:?}, {priority:?}) failed: {e}"));
            let back: TaskEnvelope = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("deserialize ({zone:?}, {priority:?}) failed: {e}"));

            assert_eq!(back.security_zone, zone);
            assert_eq!(back.priority, priority);
            assert_eq!(back.envelope_version, ENVELOPE_VERSION);
            assert_eq!(back.task_id, original.task_id);
            assert_eq!(back.parent_dag_id, original.parent_dag_id);
            assert_eq!(back.task_spec.title, original.task_spec.title);
            assert_eq!(back.task_spec.env_vars, original.task_spec.env_vars);
            assert_eq!(back.tool_endpoints.len(), original.tool_endpoints.len());
            assert_eq!(
                back.checkpoint_policy.interval,
                original.checkpoint_policy.interval
            );
            assert_eq!(
                back.checkpoint_policy.on_tool_call,
                original.checkpoint_policy.on_tool_call
            );
            assert!(
                back.validate().is_ok(),
                "post-deserialization validate failed for ({zone:?}, {priority:?})"
            );
        }
    }
}

/// Property: every combination of zone × priority × interesting-budget round-trips
/// AND honors `is_token_budget_warning` / `is_token_budget_exceeded` invariants:
///   - warning ↔ tokens >= floor(max_tokens * threshold)
///   - exceeded ↔ tokens > max_tokens
fn budget_cases() -> Vec<(&'static str, ResourceBudget)> {
    vec![
        (
            "min-nonzero",
            ResourceBudget {
                max_rss_mb: 1,
                max_wall_time: Duration::from_secs(1),
                max_tokens: 1,
                budget_warning_threshold: 0.0,
            },
        ),
        ("default", ResourceBudget::default()),
        (
            "high-threshold",
            ResourceBudget {
                max_rss_mb: 4096,
                max_wall_time: Duration::from_secs(7200),
                max_tokens: 1_000_000,
                budget_warning_threshold: 1.0,
            },
        ),
        (
            "low-threshold",
            ResourceBudget {
                max_rss_mb: 64,
                max_wall_time: Duration::from_secs(30),
                max_tokens: 10_000,
                budget_warning_threshold: 0.1,
            },
        ),
    ]
}

#[test]
fn envelope_round_trip_zone_priority_budget_matrix() {
    let cases = budget_cases();
    for zone in ALL_ZONES {
        for priority in ALL_PRIORITIES {
            for (label, budget) in &cases {
                let env = make_envelope(zone, priority, budget.clone());
                assert!(
                    env.validate().is_ok(),
                    "{label} budget must validate for ({zone:?}, {priority:?})"
                );

                let json = serde_json::to_vec(&env).expect("serialize");
                let back: TaskEnvelope = serde_json::from_slice(&json).expect("deserialize");
                assert_eq!(back.resource_budget.max_rss_mb, env.resource_budget.max_rss_mb);
                assert_eq!(back.resource_budget.max_tokens, env.resource_budget.max_tokens);
                assert_eq!(
                    back.resource_budget.max_wall_time,
                    env.resource_budget.max_wall_time
                );
                assert!(
                    (back.resource_budget.budget_warning_threshold
                        - env.resource_budget.budget_warning_threshold)
                        .abs()
                        < f64::EPSILON,
                    "{label} threshold mismatch on round-trip"
                );

                // Token-budget invariants: probe at -1 / =threshold / =max / =max+1.
                let max = env.resource_budget.max_tokens;
                let threshold_floor = (max as f64
                    * env.resource_budget.budget_warning_threshold)
                    as u64;

                assert!(
                    !env.is_token_budget_exceeded(max),
                    "{label}: max tokens must NOT be exceeded at ({zone:?}, {priority:?})"
                );
                assert!(
                    env.is_token_budget_exceeded(max + 1),
                    "{label}: max+1 tokens must be exceeded at ({zone:?}, {priority:?})"
                );
                assert!(
                    env.is_token_budget_warning(threshold_floor),
                    "{label}: threshold floor must trigger warning at ({zone:?}, {priority:?})"
                );
                if threshold_floor > 0 {
                    assert!(
                        !env.is_token_budget_warning(threshold_floor - 1),
                        "{label}: just under threshold must NOT warn at ({zone:?}, {priority:?})"
                    );
                }
            }
        }
    }
}

/// Property: validation rejects every individual zero-budget violation, regardless
/// of (zone, priority). Catches future regressions that might accidentally allow
/// a zero-field envelope to pass for some priority class (e.g., P0Critical bypass).
#[test]
fn envelope_validation_rejects_zero_budget_fields_across_matrix() {
    for zone in ALL_ZONES {
        for priority in ALL_PRIORITIES {
            let mut env = make_envelope(zone, priority, ResourceBudget::default());
            env.resource_budget.max_tokens = 0;
            assert!(
                env.validate().is_err(),
                "max_tokens=0 must be rejected for ({zone:?}, {priority:?})"
            );

            let mut env = make_envelope(zone, priority, ResourceBudget::default());
            env.resource_budget.max_rss_mb = 0;
            assert!(
                env.validate().is_err(),
                "max_rss_mb=0 must be rejected for ({zone:?}, {priority:?})"
            );

            let mut env = make_envelope(zone, priority, ResourceBudget::default());
            env.resource_budget.max_wall_time = Duration::ZERO;
            assert!(
                env.validate().is_err(),
                "max_wall_time=0 must be rejected for ({zone:?}, {priority:?})"
            );

            let mut env = make_envelope(zone, priority, ResourceBudget::default());
            env.resource_budget.budget_warning_threshold = -0.01;
            assert!(
                env.validate().is_err(),
                "negative threshold must be rejected for ({zone:?}, {priority:?})"
            );

            let mut env = make_envelope(zone, priority, ResourceBudget::default());
            env.resource_budget.budget_warning_threshold = 1.01;
            assert!(
                env.validate().is_err(),
                "threshold > 1.0 must be rejected for ({zone:?}, {priority:?})"
            );
        }
    }
}

/// Property: priority enum total ordering and serialized form are consistent.
/// P0Critical < P1Normal < P2Background and JSON shape is variant-named (not numeric),
/// so a logging downstream can rely on the string labels.
#[test]
fn priority_total_order_and_serialized_form_are_stable() {
    assert!(Priority::P0Critical < Priority::P1Normal);
    assert!(Priority::P1Normal < Priority::P2Background);
    assert!(Priority::P0Critical < Priority::P2Background);
    assert_eq!(Priority::P0Critical, Priority::P0Critical);

    for p in ALL_PRIORITIES {
        let json = serde_json::to_string(&p).expect("serialize priority");
        let label = match p {
            Priority::P0Critical => "\"P0Critical\"",
            Priority::P1Normal => "\"P1Normal\"",
            Priority::P2Background => "\"P2Background\"",
        };
        assert_eq!(json, label, "priority serialized form drifted for {p:?}");
    }
}

/// Property: SecurityZone serialization is variant-named string. Telemetry/policy
/// systems join on the string form, so changing it would silently break them.
#[test]
fn security_zone_serialized_form_is_stable() {
    for z in ALL_ZONES {
        let json = serde_json::to_string(&z).expect("serialize zone");
        let label = match z {
            SecurityZone::Green => "\"Green\"",
            SecurityZone::Yellow => "\"Yellow\"",
            SecurityZone::Red => "\"Red\"",
        };
        assert_eq!(json, label, "zone serialized form drifted for {z:?}");
    }
}
