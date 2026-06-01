use super::*;

pub(super) struct BudgetEvaluationOutputs<'a> {
    pub(super) action: &'a mut PolicyAction,
    pub(super) explanations: &'a mut Vec<DecisionExplanation>,
    pub(super) limit_hits: &'a mut Vec<DecisionLimitHitReport>,
    pub(super) message_hints: &'a mut Vec<AuthorizeMessageHint>,
}

pub(super) struct BudgetLimitEvaluation<'a> {
    pub(super) ledger: &'a mut BudgetLedger,
    pub(super) rule: &'a BudgetRule,
    pub(super) request: &'a AuthorizeRequest,
    pub(super) estimated_cost: f64,
    pub(super) now: DateTime<Utc>,
    pub(super) action: &'a mut PolicyAction,
    pub(super) explanations: &'a mut Vec<DecisionExplanation>,
    pub(super) limit_hits: &'a mut Vec<DecisionLimitHitReport>,
    pub(super) message_hints: &'a mut Vec<AuthorizeMessageHint>,
}

pub(super) fn apply_budget_limits(context: BudgetLimitEvaluation<'_>) -> bool {
    let BudgetLimitEvaluation {
        ledger,
        rule,
        request,
        estimated_cost,
        now,
        action,
        explanations,
        limit_hits,
        message_hints,
    } = context;
    if let Some(limit) = &rule.limits.request_cost
        && estimated_cost > limit.max_usd
    {
        let denied = push_limit_explanation(
            format!("{}.request_cost", rule.id),
            format!(
                "estimated request cost ${estimated_cost:.6} exceeds limit max ${:.6}",
                limit.max_usd
            ),
            format!(
                "estimated request cost ${estimated_cost:.6} exceeds enforced limit max ${:.6}",
                limit.max_usd
            ),
            limit.action,
            action,
            explanations,
        );
        message_hints.push(AuthorizeMessageHint {
            kind: "request_cost".to_owned(),
            rule_id: format!("{}.request_cost", rule.id),
            severity: limit.action.decision_severity(),
            recommendation: ledger.recommend_message_hint(
                request,
                &format!("warn.{}.request_cost", rule.id),
                "request",
                limit.action.decision_severity(),
                now,
                limit.warning_cadence.as_deref(),
            ),
            limit_type: Some("request_cost".to_owned()),
            window_id: None,
            window_label: None,
            window_mode: None,
            window_ends_at: None,
            projected_spend_usd: Some(estimated_cost),
            max_usd: Some(limit.max_usd),
            threshold_usd: None,
            threshold_percent: None,
        });
        if denied {
            return true;
        }
    }

    if let Some(limit) = &rule.limits.context_tokens
        && let Some(estimated_tokens) = request.estimated_tokens
        && estimated_tokens > limit.max_tokens
    {
        let denied = push_limit_explanation(
            format!("{}.context_tokens", rule.id),
            format!(
                "estimated context tokens {estimated_tokens} exceed limit max {}",
                limit.max_tokens
            ),
            format!(
                "estimated context tokens {estimated_tokens} exceed enforced limit max {}",
                limit.max_tokens
            ),
            limit.action,
            action,
            explanations,
        );
        message_hints.push(AuthorizeMessageHint {
            kind: "context_tokens".to_owned(),
            rule_id: format!("{}.context_tokens", rule.id),
            severity: limit.action.decision_severity(),
            recommendation: ledger.recommend_message_hint(
                request,
                &format!("warn.{}.context_tokens", rule.id),
                "context",
                limit.action.decision_severity(),
                now,
                limit.warning_cadence.as_deref(),
            ),
            limit_type: Some("context_tokens".to_owned()),
            window_id: None,
            window_label: None,
            window_mode: None,
            window_ends_at: None,
            projected_spend_usd: None,
            max_usd: None,
            threshold_usd: None,
            threshold_percent: None,
        });
        if denied {
            return true;
        }
    }

    for projection in spend_window_projections(ledger, rule, request, estimated_cost, now)
        .expect("selected budget has valid spend window scopes")
    {
        if let Some(warn_at_fraction) = newly_crossed_warn_at_fraction(&projection) {
            let warn_threshold = projection.max_usd * warn_at_fraction;
            *action = merge_policy_action(*action, PolicyAction::Warn);
            explanations.push(DecisionExplanation {
                rule_id: projection.rule_id.clone(),
                reason: format!(
                    "projected spend ${:.6} reaches warning threshold ${:.6} for {} window",
                    projection.projected_spend_usd, warn_threshold, projection.window_label
                ),
                severity: DecisionSeverity::Warn,
            });
            message_hints.push(message_hint_from_projection(
                "spend_threshold",
                &projection,
                DecisionSeverity::Warn,
                Some(warn_threshold),
                ledger.recommend_message_hint(
                    request,
                    &format!("warn.{}.threshold", projection.rule_id),
                    &projection.scope_key,
                    DecisionSeverity::Warn,
                    now,
                    projection.warning_cadence.as_deref(),
                ),
            ));
        }
        if projection.projected_spend_usd > projection.max_usd {
            let hit = spend_limit_hit(&projection);
            let denied = push_limit_explanation(
                hit.rule_id.clone(),
                hit.reason.clone(),
                hit.reason.clone(),
                projection.action,
                action,
                explanations,
            );
            message_hints.push(message_hint_from_projection(
                "spend_limit",
                &projection,
                hit.severity,
                None,
                ledger.recommend_message_hint(
                    request,
                    &format!("warn.{}", projection.rule_id),
                    &projection.scope_key,
                    hit.severity,
                    now,
                    projection.warning_cadence.as_deref(),
                ),
            ));
            limit_hits.push(hit);
            if denied {
                return true;
            }
        }
    }

    false
}

pub(super) fn message_hints_metadata(message_hints: &[AuthorizeMessageHint]) -> Option<Value> {
    authorize_metadata(message_hints, &[])
}

pub(super) fn authorize_metadata(
    message_hints: &[AuthorizeMessageHint],
    notifications: &[AuthorizeNotification],
) -> Option<Value> {
    if message_hints.is_empty() && notifications.is_empty() {
        return None;
    }
    let mut metadata = serde_json::Map::new();
    if !message_hints.is_empty() {
        metadata.insert("message_hints".to_owned(), json!(message_hints));
    }
    if !notifications.is_empty() {
        metadata.insert("notifications".to_owned(), json!(notifications));
    }
    Some(Value::Object(metadata))
}

pub(super) fn message_hint_from_projection(
    kind: &str,
    projection: &SpendWindowProjection,
    severity: DecisionSeverity,
    threshold_usd: Option<f64>,
    recommendation: MessageHintRecommendation,
) -> AuthorizeMessageHint {
    AuthorizeMessageHint {
        kind: kind.to_owned(),
        rule_id: projection.rule_id.clone(),
        severity,
        recommendation,
        limit_type: Some("spend".to_owned()),
        window_id: Some(projection.limit_id.clone()),
        window_label: Some(projection.window_label.clone()),
        window_mode: Some(match projection.limit_mode {
            SpendWindowMode::Rolling => "rolling".to_owned(),
            SpendWindowMode::Tumbling => "tumbling".to_owned(),
        }),
        window_ends_at: projection.window_ends_at,
        projected_spend_usd: Some(projection.projected_spend_usd),
        max_usd: Some(projection.max_usd),
        threshold_usd,
        threshold_percent: threshold_usd
            .map(|threshold| ((threshold / projection.max_usd) * 100.0).round() as u64),
    }
}

pub(super) fn message_hint_from_limit_hit(
    kind: &str,
    hit: &DecisionLimitHitReport,
) -> AuthorizeMessageHint {
    AuthorizeMessageHint {
        kind: kind.to_owned(),
        rule_id: hit.rule_id.clone(),
        severity: hit.severity,
        recommendation: MessageHintRecommendation::Show,
        limit_type: Some("spend".to_owned()),
        window_id: hit.window_id.clone(),
        window_label: hit.window_id.clone(),
        window_mode: hit.window_mode.clone(),
        window_ends_at: hit.window_ends_at,
        projected_spend_usd: hit.projected_spend_usd,
        max_usd: hit.max_usd,
        threshold_usd: None,
        threshold_percent: None,
    }
}

pub(super) fn spend_window_projections(
    ledger: &BudgetLedger,
    rule: &BudgetRule,
    request: &AuthorizeRequest,
    estimated_cost: f64,
    now: DateTime<Utc>,
) -> Result<Vec<SpendWindowProjection>, String> {
    rule.limits
        .spend
        .iter()
        .map(|limit| {
            let window_seconds =
                crate::policy::parse_limit_window(&limit.window).expect("validated spend window");
            let limit_id = spend_limit_identifier(limit).to_owned();
            let limit_mode = limit.mode.expect("validated spend window mode");
            let scope_key = spend_limit_scope_key(limit.by, request).ok_or_else(|| {
                format!(
                    "request is missing {} scope required by spend window {}",
                    spend_window_by_label(limit.by),
                    spend_limit_identifier(limit)
                )
            })?;
            let (current_spend, window_started_at, window_ends_at) = match limit_mode {
                SpendWindowMode::Rolling => (
                    recent_spend_usd(
                        ledger,
                        &rule.id,
                        &limit_id,
                        &scope_key,
                        now - window_seconds,
                        now,
                    ),
                    Some(now - window_seconds),
                    Some(now),
                ),
                SpendWindowMode::Tumbling => {
                    let key = (rule.id.clone(), limit_id.clone(), scope_key.clone());
                    let started_at = ledger
                        .limit_windows
                        .get(&key)
                        .map(|state| {
                            if now - state.started_at >= window_seconds {
                                advance_tumbling_window_start(state.started_at, window_seconds, now)
                            } else {
                                state.started_at
                            }
                        })
                        .unwrap_or(now);
                    (
                        ledger.limit_window_used_usd(
                            rule,
                            &limit_id,
                            window_seconds,
                            &scope_key,
                            now,
                        ),
                        Some(started_at),
                        Some(started_at + window_seconds),
                    )
                }
            };
            Ok(SpendWindowProjection {
                rule_id: format!("{}.spend_window.{}", rule.id, limit_id),
                limit_id,
                window_label: limit.window.clone(),
                action: limit.action,
                limit_mode,
                window_started_at,
                window_ends_at,
                current_spend_usd: current_spend,
                projected_spend_usd: current_spend + estimated_cost,
                max_usd: limit.max_usd,
                warn_at_fractions: limit.warn_at_fractions.clone(),
                warning_cadence: limit.warning_cadence.clone(),
                scope_key,
                window_seconds,
            })
        })
        .collect()
}

pub(super) fn newly_crossed_warn_at_fraction(projection: &SpendWindowProjection) -> Option<f64> {
    projection
        .warn_at_fractions
        .iter()
        .copied()
        .filter(|warn_at_fraction| *warn_at_fraction < 1.0)
        .filter(|warn_at_fraction| {
            let threshold = projection.max_usd * *warn_at_fraction;
            projection.current_spend_usd < threshold && projection.projected_spend_usd >= threshold
        })
        .max_by(|left, right| left.total_cmp(right))
}

pub(super) fn biggest_spend_window_projection(
    ledger: &BudgetLedger,
    rule: &BudgetRule,
    request: &AuthorizeRequest,
    estimated_cost: f64,
    now: DateTime<Utc>,
) -> Option<SpendWindowProjection> {
    spend_window_projections(ledger, rule, request, estimated_cost, now)
        .ok()?
        .into_iter()
        .max_by_key(|projection| projection.window_seconds.num_seconds())
}

pub(super) fn biggest_spend_window_duration(rule: &BudgetRule) -> Option<Duration> {
    rule.limits
        .spend
        .iter()
        .filter_map(|limit| crate::policy::parse_limit_window(&limit.window))
        .max_by_key(|window| window.num_seconds())
}

pub(super) fn biggest_policy_rolling_spend_window_duration(
    policy: &PolicyFile,
) -> Option<Duration> {
    policy
        .budgets
        .iter()
        .flat_map(|rule| &rule.limits.spend)
        .filter(|limit| matches!(limit.mode, Some(SpendWindowMode::Rolling)))
        .filter_map(|limit| crate::policy::parse_limit_window(&limit.window))
        .max_by_key(|window| window.num_seconds())
}

pub(super) fn spend_limit_hit(projection: &SpendWindowProjection) -> DecisionLimitHitReport {
    let reason = match projection.action {
        PolicyAction::Warn => format!(
            "projected spend ${:.6} exceeds {} limit max ${:.6}",
            projection.projected_spend_usd, projection.window_label, projection.max_usd
        ),
        PolicyAction::Ask | PolicyAction::Block => format!(
            "projected spend ${:.6} exceeds enforced {} limit max ${:.6}",
            projection.projected_spend_usd, projection.window_label, projection.max_usd
        ),
        PolicyAction::Allow => unreachable!("limit validation forbids allow actions"),
    };
    DecisionLimitHitReport {
        rule_id: projection.rule_id.clone(),
        reason,
        severity: match projection.action {
            PolicyAction::Warn => DecisionSeverity::Warn,
            PolicyAction::Ask | PolicyAction::Block => DecisionSeverity::Deny,
            PolicyAction::Allow => unreachable!("limit validation forbids allow actions"),
        },
        window_id: Some(projection.limit_id.clone()),
        window_mode: Some(match projection.limit_mode {
            SpendWindowMode::Rolling => "rolling".to_owned(),
            SpendWindowMode::Tumbling => "tumbling".to_owned(),
        }),
        window_started_at: projection.window_started_at,
        window_ends_at: projection.window_ends_at,
        projected_spend_usd: Some(projection.projected_spend_usd),
        max_usd: Some(projection.max_usd),
        scope_entity: Some(projection.scope_key.clone()),
    }
}

pub(super) fn push_limit_explanation(
    rule_id: String,
    warn_reason: String,
    deny_reason: String,
    action: PolicyAction,
    current_action: &mut PolicyAction,
    explanations: &mut Vec<DecisionExplanation>,
) -> bool {
    let (severity, reason, denied) = match action {
        PolicyAction::Warn => (DecisionSeverity::Warn, warn_reason, false),
        PolicyAction::Ask | PolicyAction::Block => (DecisionSeverity::Deny, deny_reason, true),
        PolicyAction::Allow => unreachable!("limit validation forbids allow actions"),
    };
    *current_action = merge_policy_action(*current_action, action);
    explanations.push(DecisionExplanation {
        rule_id,
        reason,
        severity,
    });
    denied
}

pub(super) fn spend_limit_identifier(limit: &crate::contract::SpendWindowLimit) -> &str {
    limit.id.as_deref().unwrap_or(limit.window.as_str())
}

pub(super) fn limit_hit_identifier(hit: &DecisionLimitHitReport) -> &str {
    hit.window_id.as_deref().unwrap_or(hit.rule_id.as_str())
}

pub(super) fn limit_hit_overflow(hit: &DecisionLimitHitReport) -> Option<f64> {
    Some(hit.projected_spend_usd? - hit.max_usd?)
}

pub(super) fn limit_hit_severity_rank(hit: &DecisionLimitHitReport) -> u8 {
    match hit.severity {
        DecisionSeverity::Deny => 0,
        DecisionSeverity::Warn => 1,
        DecisionSeverity::Info => 2,
    }
}

pub fn binding_limit_hit(hits: &[DecisionLimitHitReport]) -> Option<&DecisionLimitHitReport> {
    hits.iter().min_by(|left, right| {
        limit_hit_severity_rank(left)
            .cmp(&limit_hit_severity_rank(right))
            .then_with(
                || match (limit_hit_overflow(left), limit_hit_overflow(right)) {
                    (Some(left_overflow), Some(right_overflow)) => {
                        right_overflow.total_cmp(&left_overflow)
                    }
                    _ => std::cmp::Ordering::Equal,
                },
            )
            .then_with(|| limit_hit_identifier(left).cmp(limit_hit_identifier(right)))
            .then_with(|| left.rule_id.cmp(&right.rule_id))
    })
}

pub(super) fn spend_limit_scope_key(
    by: SpendWindowBy,
    request: &AuthorizeRequest,
) -> Option<String> {
    match by {
        SpendWindowBy::Global => Some("global".to_owned()),
        SpendWindowBy::Project => request
            .project
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|project| format!("project:{project}"))
            .or_else(|| first_request_entity(request, "project")),
        SpendWindowBy::User => request
            .entities
            .iter()
            .find(|entity| entity.starts_with("user:"))
            .cloned()
            .or_else(|| {
                request
                    .subject
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .map(|subject| {
                        if subject.contains(':') {
                            subject.to_owned()
                        } else {
                            format!("user:{subject}")
                        }
                    })
            }),
        SpendWindowBy::Team => first_request_entity(request, "team"),
        SpendWindowBy::Group => first_request_entity(request, "group"),
        SpendWindowBy::Org => first_request_entity(request, "org"),
        SpendWindowBy::Workflow => first_request_entity(request, "workflow"),
        SpendWindowBy::Surface => first_request_entity(request, "surface"),
    }
}

pub(super) fn request_user_key(request: &AuthorizeRequest) -> String {
    request
        .subject
        .as_deref()
        .map(normalized_user_entity)
        .or_else(|| first_request_entity(request, "user"))
        .unwrap_or_else(|| "anonymous".to_owned())
}

pub(super) fn normalized_user_entity(value: &str) -> String {
    if value.contains(':') {
        value.to_owned()
    } else {
        format!("user:{value}")
    }
}

pub(super) fn spend_window_by_label(by: SpendWindowBy) -> &'static str {
    match by {
        SpendWindowBy::Global => "global",
        SpendWindowBy::Project => "project",
        SpendWindowBy::User => "user",
        SpendWindowBy::Team => "team",
        SpendWindowBy::Group => "group",
        SpendWindowBy::Org => "org",
        SpendWindowBy::Workflow => "workflow",
        SpendWindowBy::Surface => "surface",
    }
}

pub(super) fn first_request_entity(request: &AuthorizeRequest, kind: &str) -> Option<String> {
    let prefix = format!("{kind}:");
    request
        .entities
        .iter()
        .find(|entity| entity.starts_with(&prefix))
        .cloned()
}
