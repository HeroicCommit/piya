use anyhow::Result;
use serde_json::Value;

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
        plan: &Value,
        execution: &Value,
        research_context: &str,
        domains: &[Domain],
    ) -> Result<Value> {
        let user = format!(
            "{}\n\nTask: {task_title}\n\nPlan:\n{plan}\n\nExecution result:\n{execution}\n\n\
             Research used:\n{research_context}\n\nReview the work.",
            expertise::format_domain_context(domains),
            plan = serde_json::to_string_pretty(plan)?,
            execution = serde_json::to_string_pretty(execution)?
        );
        self.llm.chat_json(&expertise::reviewer_system(), &user).await
    }
}
