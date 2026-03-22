use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    Idle,
    Busy,
    Done,
    Failed,
}

impl AgentState {
    pub fn can_execute(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn transition_to_busy(&mut self) -> Result<(), &'static str> {
        if *self != Self::Idle {
            return Err("Can only start from Idle");
        }
        *self = Self::Busy;
        Ok(())
    }

    pub fn transition_to_done(&mut self) -> Result<(), &'static str> {
        if *self != Self::Busy {
            return Err("Can only complete from Busy");
        }
        *self = Self::Done;
        Ok(())
    }

    pub fn transition_to_failed(&mut self) -> Result<(), &'static str> {
        if *self != Self::Busy {
            return Err("Can only fail from Busy");
        }
        *self = Self::Failed;
        Ok(())
    }

    pub fn reset(&mut self) {
        *self = Self::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_is_idle() {
        let state = AgentState::Idle;
        assert_eq!(state, AgentState::Idle);
        assert!(state.can_execute());
    }

    #[test]
    fn test_idle_to_busy_transition() {
        let mut state = AgentState::Idle;
        assert!(state.transition_to_busy().is_ok());
        assert_eq!(state, AgentState::Busy);
        assert!(!state.can_execute());
    }

    #[test]
    fn test_busy_to_done_transition() {
        let mut state = AgentState::Idle;
        state.transition_to_busy().unwrap();
        assert!(state.transition_to_done().is_ok());
        assert_eq!(state, AgentState::Done);
    }

    #[test]
    fn test_cannot_execute_when_busy() {
        let mut state = AgentState::Idle;
        state.transition_to_busy().unwrap();
        assert!(!state.can_execute());
        assert!(state.transition_to_busy().is_err());
    }

    #[test]
    fn test_reset_returns_to_idle() {
        let mut state = AgentState::Idle;
        state.transition_to_busy().unwrap();
        state.transition_to_failed().unwrap();
        assert_eq!(state, AgentState::Failed);
        state.reset();
        assert_eq!(state, AgentState::Idle);
        assert!(state.can_execute());
    }
}
