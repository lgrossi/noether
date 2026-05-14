use std::collections::HashMap;

use chrono::{Duration, Utc};
use uuid::Uuid;

use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, BudgetRule, DecisionExplanation, DecisionOutcome,
    DecisionSeverity, FinalizeReservation, PolicyEffect, Reservation, ReservationStatus,
    TraceEvent,
};
use crate::error::NoetError;
use crate::policy::{PolicyFile, matching_policy_explanations, rule_match_matches};

#[derive(Debug, Default)]
pub struct BudgetLedger {
    windows: HashMap<String, WindowState>,
    reservations: HashMap<String, StoredReservation>,
    events: Vec<TraceEvent>,
}

#[derive(Debug)]
struct WindowState {
    started_at: chrono::DateTime<Utc>,
    used_usd: f64,
}

#[derive(Debug)]
struct StoredReservation {
    reservation: Reservation,
    estimated_cost_usd: f64,
    budget_rule_ids: Vec<String>,
}

impl BudgetLedger {
    pub fn authorize(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
    ) -> AuthorizeDecision {
        let now = Utc::now();
        let mut outcome = DecisionOutcome::Allow;
        let mut explanations = Vec::new();

        if let Some(policy) = policy {
            for (effect, explanation) in matching_policy_explanations(policy, request) {
                outcome = merge_policy_outcome(outcome, effect);
                explanations.push(explanation);
            }

            if outcome != DecisionOutcome::Deny {
                self.evaluate_budget_rules(policy, request, now, &mut outcome, &mut explanations);
            }
        } else {
            explanations.push(DecisionExplanation {
                rule_id: "no_policy".to_owned(),
                reason: "no policy file configured; request allowed".to_owned(),
                severity: DecisionSeverity::Info,
            });
        }

        let reservation = if outcome == DecisionOutcome::Deny {
            None
        } else {
            Some(self.create_reservation(policy, request, now))
        };

        AuthorizeDecision {
            decision_id: Uuid::new_v4().to_string(),
            outcome,
            reservation,
            explanations,
            created_at: now,
        }
    }

    pub fn finalize(
        &mut self,
        reservation_id: &str,
        payload: &FinalizeReservation,
    ) -> Result<Reservation, NoetError> {
        let stored = self
            .reservations
            .get_mut(reservation_id)
            .ok_or_else(|| NoetError::NotFound(format!("reservation {reservation_id}")))?;

        if stored.reservation.status == ReservationStatus::Finalized {
            return Ok(stored.reservation.clone());
        }

        let actual_cost = payload
            .actual_cost_usd
            .or_else(|| payload.usage.as_ref().and_then(|usage| usage.cost_usd));
        if let Some(actual_cost) = actual_cost {
            let delta = actual_cost - stored.estimated_cost_usd;
            for rule_id in &stored.budget_rule_ids {
                if let Some(window) = self.windows.get_mut(rule_id) {
                    window.used_usd = (window.used_usd + delta).max(0.0);
                }
            }
            stored.reservation.amount_usd = actual_cost;
        }

        stored.reservation.status = ReservationStatus::Finalized;
        Ok(stored.reservation.clone())
    }

    pub fn record_event(&mut self, event: TraceEvent) {
        self.events.push(event);
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    fn evaluate_budget_rules(
        &mut self,
        policy: &PolicyFile,
        request: &AuthorizeRequest,
        now: chrono::DateTime<Utc>,
        outcome: &mut DecisionOutcome,
        explanations: &mut Vec<DecisionExplanation>,
    ) {
        let estimated_cost = request.estimated_cost();
        let matching_rules: Vec<&BudgetRule> = policy
            .budgets
            .iter()
            .filter(|rule| rule_match_matches(&rule.rule_match, request))
            .collect();

        if matching_rules.is_empty() {
            explanations.push(DecisionExplanation {
                rule_id: "no_budget_match".to_owned(),
                reason: "no matching budget rule; request allowed".to_owned(),
                severity: DecisionSeverity::Info,
            });
            return;
        }

        for rule in matching_rules {
            let window = self.window(rule, now);
            let projected = window.used_usd + estimated_cost;
            if projected > rule.limit_usd {
                *outcome = DecisionOutcome::Deny;
                explanations.push(DecisionExplanation {
                    rule_id: rule.id.clone(),
                    reason: format!(
                        "estimated cost ${projected:.6} exceeds fixed-window limit ${:.6}",
                        rule.limit_usd
                    ),
                    severity: DecisionSeverity::Deny,
                });
            } else if projected >= rule.limit_usd * rule.warn_at_fraction
                && *outcome != DecisionOutcome::Deny
            {
                *outcome = DecisionOutcome::Warn;
                explanations.push(DecisionExplanation {
                    rule_id: rule.id.clone(),
                    reason: format!(
                        "estimated cost ${projected:.6} reaches warning threshold ${:.6}",
                        rule.limit_usd * rule.warn_at_fraction
                    ),
                    severity: DecisionSeverity::Warn,
                });
            }
        }
    }

    fn create_reservation(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        now: chrono::DateTime<Utc>,
    ) -> Reservation {
        let amount_usd = request.estimated_cost();
        let matching_rules: Vec<&BudgetRule> = policy
            .map(|policy| {
                policy
                    .budgets
                    .iter()
                    .filter(|rule| rule_match_matches(&rule.rule_match, request))
                    .collect()
            })
            .unwrap_or_default();
        let budget_rule_ids: Vec<String> =
            matching_rules.iter().map(|rule| rule.id.clone()).collect();
        let expires_at = matching_rules
            .iter()
            .map(|rule| now + Duration::seconds(rule.window_seconds))
            .min()
            .unwrap_or_else(|| now + Duration::hours(1));

        for rule in matching_rules {
            self.window(rule, now).used_usd += amount_usd;
        }

        let reservation = Reservation {
            id: Uuid::new_v4().to_string(),
            amount_usd,
            currency: "USD".to_owned(),
            status: ReservationStatus::Active,
            created_at: now,
            expires_at,
        };
        self.reservations.insert(
            reservation.id.clone(),
            StoredReservation {
                reservation: reservation.clone(),
                estimated_cost_usd: amount_usd,
                budget_rule_ids,
            },
        );
        reservation
    }

    fn window(&mut self, rule: &BudgetRule, now: chrono::DateTime<Utc>) -> &mut WindowState {
        let window_seconds = Duration::seconds(rule.window_seconds);
        let window = self.windows.entry(rule.id.clone()).or_insert(WindowState {
            started_at: now,
            used_usd: 0.0,
        });

        if now - window.started_at >= window_seconds {
            window.started_at = now;
            window.used_usd = 0.0;
        }

        window
    }
}

fn merge_policy_outcome(current: DecisionOutcome, effect: PolicyEffect) -> DecisionOutcome {
    if current == DecisionOutcome::Deny || effect == PolicyEffect::Deny {
        DecisionOutcome::Deny
    } else if current == DecisionOutcome::Warn || effect == PolicyEffect::Warn {
        DecisionOutcome::Warn
    } else {
        DecisionOutcome::Allow
    }
}

#[cfg(test)]
mod tests {
    use crate::contract::{BudgetRule, RuleMatch};

    use super::*;

    fn policy(limit_usd: f64, warn_at_fraction: f64) -> PolicyFile {
        PolicyFile {
            version: 0,
            budgets: vec![BudgetRule {
                id: "dev-budget".to_owned(),
                limit_usd,
                warn_at_fraction,
                window_seconds: 60,
                rule_match: RuleMatch {
                    project: Some("noether".to_owned()),
                    ..RuleMatch::default()
                },
            }],
            policies: Vec::new(),
        }
    }

    fn request(cost: f64) -> AuthorizeRequest {
        AuthorizeRequest {
            project: Some("noether".to_owned()),
            estimated_cost_usd: Some(cost),
            subject: None,
            provider: None,
            model: None,
            estimated_tokens: None,
            metadata: Default::default(),
        }
    }

    #[test]
    fn budget_evaluator_allows_with_reservation_under_threshold() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.25));

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert!(decision.reservation.is_some());
    }

    #[test]
    fn budget_evaluator_warns_at_threshold() {
        let policy = policy(1.0, 0.5);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.50));

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget" && explanation.severity == DecisionSeverity::Warn
        }));
    }

    #[test]
    fn budget_evaluator_denies_over_limit() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(1.01));

        assert_eq!(decision.outcome, DecisionOutcome::Deny);
        assert!(decision.reservation.is_none());
    }

    #[test]
    fn finalize_is_idempotent() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();
        let decision = ledger.authorize(Some(&policy), &request(0.25));
        let reservation_id = decision.reservation.expect("reservation").id;
        let payload = FinalizeReservation {
            reservation_id: None,
            usage: None,
            actual_cost_usd: Some(0.20),
        };

        let first = ledger
            .finalize(&reservation_id, &payload)
            .expect("first finalize");
        let second = ledger
            .finalize(&reservation_id, &payload)
            .expect("second finalize");

        assert_eq!(first.status, ReservationStatus::Finalized);
        assert_eq!(second.amount_usd, 0.20);
    }
}
