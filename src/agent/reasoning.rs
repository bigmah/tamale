/// Anthropic's effort setting (`output_config.effort`): trades thoroughness for tokens and
/// latency. Not every model supports every level; the API rejects unsupported ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// Parses a level name; `None` for unknown names.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str().eq_ignore_ascii_case(s.trim()))
    }

    pub fn parse_setting(s: &str) -> anyhow::Result<Option<Self>> {
        use anyhow::Context;
        if matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "default" | "auto" | "none" | "reset"
        ) {
            return Ok(None);
        }
        Self::parse(s).map(Some).with_context(|| {
            format!("unknown effort level: {s} (use low, medium, high, xhigh, max, or default)")
        })
    }
}

/// Anthropic's adaptive thinking, controlled independently of effort.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Thinking {
    #[default]
    Adaptive,
    Off,
}

impl Thinking {
    pub fn parse_setting(s: &str) -> anyhow::Result<Self> {
        use anyhow::Context;
        Self::parse(s)
            .with_context(|| format!("unknown thinking mode: {s} (use adaptive, on, off)"))
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "adaptive" | "on" => Some(Self::Adaptive),
            "off" | "disabled" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Adaptive => "adaptive",
            Self::Off => "off",
        }
    }
}

/// Which API the reasoning settings are sent to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Dialect {
    #[default]
    Anthropic,
    /// OpenAI's Chat Completions, as compatible providers speak it.
    OpenAi,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Reasoning {
    pub thinking: Thinking,
    /// `None` leaves the API default.
    pub effort: Option<Effort>,
    pub dialect: Dialect,
}

impl Reasoning {
    pub(super) fn params(self) -> serde_json::Value {
        if self.dialect == Dialect::OpenAi {
            // Compatible APIs have no common thinking switch; `reasoning_effort` is the
            // closest there is, and only sent when asked for, since not all accept it.
            return match self.effort {
                Some(effort) => serde_json::json!({ "reasoning_effort": match effort {
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    Effort::High | Effort::XHigh | Effort::Max => "high",
                } }),
                None => serde_json::json!({}),
            };
        }
        let mut params = serde_json::json!({
            "thinking": { "type": match self.thinking {
                Thinking::Adaptive => "adaptive",
                Thinking::Off => "disabled",
            } }
        });
        if let Some(effort) = self.effort {
            params["output_config"] = serde_json::json!({ "effort": effort.as_str() });
        }
        params
    }
}
