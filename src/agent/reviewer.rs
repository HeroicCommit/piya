use anyhow::Result;
use serde_json::{json, Value};

use crate::agent::expertise::{self, Domain};
use crate::llm::LlmClient;

pub struct Reviewer {
    llm: LlmClient,
}

impl Reviewer {
    pub fn new(llm: LlmClient) -> Self {
        Self { llm }
    }

    pub async fn review(
        &self,
        task_title: &str,
        done_when: Option<&str>,
        plan: &Value,
        execution: &Value,
        research_context: &str,
        domains: &[Domain],
    ) -> Result<Value> {
        let evidence = Evidence::from_execution(execution);
        let user = format!(
            "{}\n\nTask: {task_title}\n\nCompletion condition (done_when): {}\n\nPlan:\n{plan}\n\n\
             Tool evidence (ground truth, from the tools — not the executor's words):\n{evidence}\n\n\
             Execution result (the executor's own summary is a CLAIM, not proof):\n{execution}\n\n\
             Research used:\n{research_context}\n\nReview the work.",
            expertise::format_domain_context(domains),
            done_when.unwrap_or("(not set — use the plan's success criteria)"),
            plan = serde_json::to_string_pretty(plan)?,
            evidence = evidence.describe(),
            execution = serde_json::to_string_pretty(execution)?
        );
        let mut review = self.llm.chat_json(&expertise::reviewer_system(), &user).await?;
        apply_evidence_gate(&mut review, &evidence);
        Ok(review)
    }
}

/// What the tools actually did during Act, counted from their recorded outputs.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Evidence {
    pub artifacts: usize,
    pub commands_ok: usize,
    pub commands_failed: usize,
    pub commands_skipped: usize,
}

impl Evidence {
    pub fn from_execution(execution: &Value) -> Self {
        let artifacts = execution
            .get("artifacts")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let mut ev = Evidence {
            artifacts,
            ..Default::default()
        };
        if let Some(cmds) = execution.get("command_outputs").and_then(|v| v.as_array()) {
            for c in cmds {
                let skipped = c.get("skipped").and_then(|v| v.as_bool()).unwrap_or(false);
                let code = c.get("returncode").and_then(|v| v.as_i64());
                if skipped {
                    ev.commands_skipped += 1;
                } else if code == Some(0) {
                    ev.commands_ok += 1;
                } else {
                    ev.commands_failed += 1;
                }
            }
        }
        ev
    }

    pub fn has_any_success(&self) -> bool {
        self.artifacts > 0 || self.commands_ok > 0
    }

    pub fn describe(&self) -> String {
        format!(
            "files written: {}; commands succeeded: {}; commands failed: {}; commands skipped/declined: {}",
            self.artifacts, self.commands_ok, self.commands_failed, self.commands_skipped
        )
    }
}

/// Never let a self-review pass work that has no tool evidence or that the reviewer
/// itself says misses the completion condition.
pub fn apply_evidence_gate(review: &mut Value, evidence: &Evidence) {
    if !review.is_object() {
        *review = json!({});
    }
    let mut cap: Option<(f64, String)> = None;

    if review.get("done_when_met").and_then(|v| v.as_bool()) == Some(false) {
        cap = Some((0.6, "Completion condition (done_when) not met.".into()));
    }
    if !evidence.has_any_success() {
        let why = if evidence.commands_failed + evidence.commands_skipped > 0 {
            format!(
                "No tool succeeded ({} failed, {} skipped/declined) — the result is unverified.",
                evidence.commands_failed, evidence.commands_skipped
            )
        } else {
            "No tool evidence: nothing was written or run, so the claimed result is unverified.".into()
        };
        cap = Some((cap.map(|c| c.0).unwrap_or(1.0).min(0.5), why));
    }

    let Some((max_acc, why)) = cap else {
        return;
    };
    let obj = review.as_object_mut().expect("object");
    let acc = obj.get("accuracy").and_then(|v| v.as_f64()).unwrap_or(0.0);
    obj.insert("accuracy".into(), json!(acc.min(max_acc)));
    obj.insert("passed".into(), json!(false));
    obj.insert("evidence_gate".into(), json!(why));
    let gaps = obj.entry("gaps").or_insert_with(|| json!([]));
    if let Some(arr) = gaps.as_array_mut() {
        arr.push(json!(why));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimed_success_without_tools_fails() {
        let exec = json!({"summary": "The reminder was successfully removed.", "artifacts": [], "command_outputs": []});
        let ev = Evidence::from_execution(&exec);
        let mut review = json!({"accuracy": 0.85, "passed": true, "gaps": []});
        apply_evidence_gate(&mut review, &ev);
        assert_eq!(review["passed"], false);
        assert!(review["accuracy"].as_f64().unwrap() <= 0.5);
    }

    #[test]
    fn declined_commands_are_not_evidence() {
        let exec = json!({"command_outputs": [
            {"cmd": "pytest", "skipped": true, "returncode": null},
            {"cmd": "python3 x.py", "skipped": false, "returncode": 1}
        ]});
        let ev = Evidence::from_execution(&exec);
        assert_eq!(ev.commands_skipped, 1);
        assert_eq!(ev.commands_failed, 1);
        let mut review = json!({"accuracy": 0.9, "passed": true});
        apply_evidence_gate(&mut review, &ev);
        assert_eq!(review["passed"], false);
    }

    #[test]
    fn done_when_not_met_fails_even_with_files() {
        let exec = json!({"artifacts": [{"path": "a.md"}]});
        let ev = Evidence::from_execution(&exec);
        let mut review = json!({"accuracy": 0.9, "passed": true, "done_when_met": false});
        apply_evidence_gate(&mut review, &ev);
        assert_eq!(review["passed"], false);
        assert!(review["accuracy"].as_f64().unwrap() <= 0.6);
    }

    #[test]
    fn real_work_passes_through() {
        let exec = json!({"artifacts": [{"path": "report.md"}],
                          "command_outputs": [{"skipped": false, "returncode": 0}]});
        let ev = Evidence::from_execution(&exec);
        let mut review = json!({"accuracy": 0.9, "passed": true, "done_when_met": true});
        apply_evidence_gate(&mut review, &ev);
        assert_eq!(review["passed"], true);
        assert_eq!(review["accuracy"], 0.9);
    }
}
