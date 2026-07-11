use echo_agent::human_loop::RiskLevel;
use echo_agent::prelude::ToolParameters;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{BrowserAction, BrowserError, BrowserResult};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserActionRisk {
    None,
    SensitiveSubmit,
    Purchase,
    Publish,
    SendMessage,
    AccountChange,
    PermissionChange,
    CloudDelete,
}

impl BrowserActionRisk {
    pub fn classify(action: BrowserAction, params: &ToolParameters) -> BrowserResult<Self> {
        if !matches!(
            action,
            BrowserAction::Click
                | BrowserAction::Fill
                | BrowserAction::ClickAt
                | BrowserAction::TypeAt
        ) {
            return Ok(Self::None);
        }
        let effect = params
            .get("effect")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::Tool {
                tool: action.name().to_string(),
                message: "effect is required; use 'none' for ordinary browser interaction"
                    .to_string(),
            })?;
        let risk = match effect {
            "none" => Self::None,
            "sensitive_submit" => Self::SensitiveSubmit,
            "purchase" => Self::Purchase,
            "publish" => Self::Publish,
            "send_message" => Self::SendMessage,
            "account_change" => Self::AccountChange,
            "permission_change" => Self::PermissionChange,
            "cloud_delete" => Self::CloudDelete,
            other => {
                return Err(BrowserError::Tool {
                    tool: action.name().to_string(),
                    message: format!("unsupported browser action effect '{other}'"),
                });
            }
        };
        if risk != Self::None && !Self::can_commit(action, params) {
            return Err(BrowserError::Tool {
                tool: action.name().to_string(),
                message: "consequential effect requires a committing click or submit action"
                    .to_string(),
            });
        }
        Ok(risk)
    }

    fn can_commit(action: BrowserAction, params: &ToolParameters) -> bool {
        match action {
            BrowserAction::Click | BrowserAction::ClickAt => true,
            BrowserAction::Fill => params
                .get("submit")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            BrowserAction::TypeAt => params
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains('\n')),
            _ => false,
        }
    }

    pub fn requires_confirmation(self) -> bool {
        self != Self::None
    }

    pub fn risk_level(self) -> RiskLevel {
        match self {
            Self::CloudDelete | Self::PermissionChange => RiskLevel::Critical,
            Self::None => RiskLevel::Low,
            _ => RiskLevel::High,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "ordinary interaction",
            Self::SensitiveSubmit => "submit sensitive information",
            Self::Purchase => "purchase or payment",
            Self::Publish => "publish content",
            Self::SendMessage => "send a message",
            Self::AccountChange => "change account settings",
            Self::PermissionChange => "change permissions",
            Self::CloudDelete => "delete cloud data",
        }
    }

    pub fn confirmation_args(self, action: BrowserAction, params: &ToolParameters) -> Value {
        json!({
            "risk": self,
            "action": action.name(),
            "summary": confirmation_text(params, "confirmationSummary", self.label()),
            "destination": confirmation_text(params, "destination", "current page"),
            "dataCategories": data_categories(params),
        })
    }

    pub fn prompt(self, params: &ToolParameters) -> String {
        let summary = confirmation_text(params, "confirmationSummary", self.label());
        let destination = confirmation_text(params, "destination", "the current page");
        let categories = data_categories(params)
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let categories = if categories.is_empty() {
            "none declared".to_string()
        } else {
            categories
        };
        format!(
            "Confirm browser action: {summary}. Destination: {destination}. Data categories: {categories}."
        )
    }
}

fn confirmation_text(params: &ToolParameters, key: &str, fallback: &str) -> String {
    let value = params.get(key).and_then(Value::as_str).unwrap_or(fallback);
    sanitize_confirmation_text(value)
}

fn data_categories(params: &ToolParameters) -> Vec<Value> {
    params
        .get("dataCategories")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(sanitize_confirmation_text)
                .filter(|value| !value.is_empty())
                .map(Value::String)
                .collect()
        })
        .unwrap_or_default()
}

fn sanitize_confirmation_text(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if [
        "authorization:",
        "cookie:",
        "set-cookie:",
        "api_key=",
        "apikey=",
        "password=",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "[redacted sensitive value]".to_string();
    }
    value.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_click_needs_no_confirmation() -> Result<(), String> {
        let params =
            ToolParameters::from([("effect".to_string(), Value::String("none".to_string()))]);
        let risk = BrowserActionRisk::classify(BrowserAction::Click, &params)
            .map_err(|error| error.to_string())?;
        assert_eq!(risk, BrowserActionRisk::None);
        Ok(())
    }

    #[test]
    fn purchase_click_is_high_risk_without_exposing_form_values() -> Result<(), String> {
        let params = ToolParameters::from([
            ("effect".to_string(), Value::String("purchase".to_string())),
            (
                "confirmationSummary".to_string(),
                Value::String("Place order for 18 USD".to_string()),
            ),
            (
                "text".to_string(),
                Value::String("secret-card-number".to_string()),
            ),
        ]);
        let risk = BrowserActionRisk::classify(BrowserAction::Click, &params)
            .map_err(|error| error.to_string())?;
        let display = risk.confirmation_args(BrowserAction::Click, &params);
        assert_eq!(risk.risk_level(), RiskLevel::High);
        assert!(!display.to_string().contains("secret-card-number"));
        Ok(())
    }

    #[test]
    fn unsubmitted_fill_cannot_claim_a_consequential_effect() {
        let params = ToolParameters::from([
            (
                "effect".to_string(),
                Value::String("sensitive_submit".to_string()),
            ),
            ("submit".to_string(), Value::Bool(false)),
        ]);
        assert!(BrowserActionRisk::classify(BrowserAction::Fill, &params).is_err());
    }

    #[test]
    fn confirmation_metadata_is_bounded_and_redacted() {
        let params = ToolParameters::from([
            (
                "confirmationSummary".to_string(),
                Value::String("Authorization: Bearer secret".to_string()),
            ),
            ("destination".to_string(), Value::String("界".repeat(400))),
        ]);
        let display =
            BrowserActionRisk::SendMessage.confirmation_args(BrowserAction::Click, &params);
        assert_eq!(
            display.get("summary").and_then(Value::as_str),
            Some("[redacted sensitive value]")
        );
        assert_eq!(
            display
                .get("destination")
                .and_then(Value::as_str)
                .map(|value| value.chars().count()),
            Some(300)
        );
    }
}
