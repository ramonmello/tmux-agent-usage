use crate::render::{FIVE_HOURS, Gauge, SEVEN_DAYS};
use crate::{claude, codex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentId {
    Claude,
    Codex,
}

impl AgentId {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(format!("unknown agent {name:?}; expected claude or codex")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    pub fn load(self) -> Usage {
        match self {
            Self::Claude => Usage::Claude(claude::load_with_time()),
            Self::Codex => Usage::Codex(codex::load_status()),
        }
    }
}

pub enum Usage {
    Claude(Option<(claude::Limits, i64)>),
    Codex(Option<codex::Status>),
}

impl Usage {
    pub fn id(&self) -> AgentId {
        match self {
            Self::Claude(_) => AgentId::Claude,
            Self::Codex(_) => AgentId::Codex,
        }
    }

    pub fn observed_at(&self) -> Option<i64> {
        match self {
            Self::Claude(data) => data.as_ref().map(|(_, at)| *at),
            Self::Codex(data) => data.as_ref().and_then(|data| data.observed_at),
        }
    }

    pub fn gauges(&self) -> Option<Vec<Option<Gauge>>> {
        match self {
            Self::Claude(data) => data.as_ref().map(|(limits, _)| {
                [
                    (limits.five_hour, FIVE_HOURS),
                    (limits.seven_day, SEVEN_DAYS),
                ]
                .into_iter()
                .map(|(window, window_secs)| {
                    window.map(|window| Gauge {
                        used_percent: window.used_percentage,
                        resets_at: window.resets_at,
                        window_secs,
                    })
                })
                .collect()
            }),
            Self::Codex(data) => data.as_ref().map(|data| {
                data.windows
                    .iter()
                    .map(|window| {
                        Some(Gauge {
                            used_percent: window.used_percent,
                            resets_at: window.resets_at,
                            window_secs: window.window_secs,
                        })
                    })
                    .collect()
            }),
        }
    }
}
