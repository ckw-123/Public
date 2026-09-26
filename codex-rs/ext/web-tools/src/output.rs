//! Model-facing output for the web tools.

use codex_extension_api::ToolOutput;
use codex_extension_api::ToolPayload;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;

/// Plain-text tool output. `success` is surfaced for logging/telemetry; the
/// model always receives the full text (including the failure contract).
pub(crate) struct WebToolOutput {
    text: String,
    success: bool,
}

impl WebToolOutput {
    pub(crate) fn ok(text: String) -> Self {
        Self {
            text,
            success: true,
        }
    }

    pub(crate) fn failed(text: String) -> Self {
        Self {
            text,
            success: false,
        }
    }
}

impl ToolOutput for WebToolOutput {
    fn log_output(&self) -> String {
        self.text.chars().take(2_000).collect()
    }

    fn success_for_logging(&self) -> bool {
        self.success
    }

    fn contains_external_context(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(self.text.clone()),
                success: Some(self.success),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::WebToolOutput;
    use codex_extension_api::ToolOutput;
    use codex_extension_api::ToolPayload;
    use codex_protocol::models::FunctionCallOutputBody;
    use codex_protocol::models::ResponseInputItem;

    #[test]
    fn emits_text_output_with_success_flag() {
        let output = WebToolOutput::failed("all engines failed".to_string());
        let item = output.to_response_item(
            "call-1",
            &ToolPayload::Function {
                arguments: "{}".to_string(),
            },
        );
        let ResponseInputItem::FunctionCallOutput { output, .. } = item else {
            panic!("expected function call output");
        };
        assert_eq!(
            output.body,
            FunctionCallOutputBody::Text("all engines failed".to_string())
        );
        assert_eq!(output.success, Some(false));
    }
}
