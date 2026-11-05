use anyhow::Result;
use serde_json::Value;

use crate::agent::expertise::{self, Domain};
use crate::llm::LlmClient;

pub struct Planner {
    llm: LlmClient,
}

impl Planner {
    pub fn new(llm: LlmClient) -> Self {
        Self { llm }
    }

    pub async fn plan(
        &self,
        title: &str,
        description: &str,
        memory_context: &str,
        domains: &[Domain],
    ) -> Result<Value> {
        let user = format!(
            "{}\n\nTask title: {title}\nTask description:\n{description}\n\n{memory_context}",
            expertise::format_domain_context(domains)
        );
        self.llm.chat_json(&expertise::planner_system(), &user).await
    }

    pub async fn replan(
        &self,
        title: &str,
        description: &str,
        memory_context: &str,
        review: &Value,
        prior_plan: &Value,
        domains: &[Domain],
    ) -> Result<Value> {
        let user = format!(
            "{}\n\nTask title: {title}\nTask description:\n{description}\n\n{memory_context}\n\n\
             Previous plan:\n{prior}\n\nReview feedback (accuracy too low, retry):\n{review}\n\n\
             Produce an improved plan addressing the review.",
            expertise::format_domain_context(domains),
            prior = serde_json::to_string_pretty(prior_plan)?,
            review = serde_json::to_string_pretty(review)?
        );
        self.llm.chat_json(&expertise::planner_system(), &user).await
    }
}
