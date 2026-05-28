use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CheckEvaluation {
    pub status: String,
    pub reason: String,
}

pub fn evaluate(latest_exit_code: Option<i64>) -> CheckEvaluation {
    match latest_exit_code {
        Some(0) => CheckEvaluation {
            status: "passed".to_string(),
            reason: "latest command evidence exited successfully".to_string(),
        },
        Some(code) => CheckEvaluation {
            status: "failed".to_string(),
            reason: format!("latest command evidence exited with {code}"),
        },
        None => CheckEvaluation {
            status: "missing".to_string(),
            reason: "no command evidence recorded for active attempt".to_string(),
        },
    }
}
