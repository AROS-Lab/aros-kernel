use super::process::ProcessId;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SupervisorError {
    #[error("process {0:?} exceeded max restart limit")]
    MaxRestartsExceeded(ProcessId),
    #[error("process {0:?} not found")]
    ProcessNotFound(ProcessId),
    #[error("invalid state transition for {0:?}: {1} -> {2}")]
    InvalidTransition(ProcessId, String, String),
    #[error("supervisor shutting down")]
    ShuttingDown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_max_restarts() {
        let err = SupervisorError::MaxRestartsExceeded(ProcessId::Loop0Meta);
        assert!(err.to_string().contains("Loop0Meta"));
        assert!(err.to_string().contains("max restart limit"));
    }

    #[test]
    fn error_display_not_found() {
        let err = SupervisorError::ProcessNotFound(ProcessId::ModelAdapter);
        assert!(err.to_string().contains("ModelAdapter"));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn error_display_invalid_transition() {
        let err = SupervisorError::InvalidTransition(
            ProcessId::Kernel,
            "Running".into(),
            "Starting".into(),
        );
        let msg = err.to_string();
        assert!(msg.contains("Kernel"));
        assert!(msg.contains("Running"));
        assert!(msg.contains("Starting"));
    }

    #[test]
    fn error_display_shutting_down() {
        let err = SupervisorError::ShuttingDown;
        assert_eq!(err.to_string(), "supervisor shutting down");
    }
}
