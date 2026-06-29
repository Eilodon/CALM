use crate::types::IndexingPhase;

pub struct IndexStateMachine {
    phase: IndexingPhase,
}

impl IndexStateMachine {
    pub fn new() -> Self {
        Self { phase: IndexingPhase::Scanning }
    }
    pub fn current(&self) -> IndexingPhase {
        self.phase
    }
    pub fn advance(&mut self) {
        self.phase = match self.phase {
            IndexingPhase::Scanning => IndexingPhase::Parsing,
            IndexingPhase::Parsing => IndexingPhase::BuildingEdges,
            IndexingPhase::BuildingEdges => IndexingPhase::Ready,
            IndexingPhase::Ready => IndexingPhase::Ready,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::IndexingPhase;

    #[test]
    fn test_phase_transition() {
        let mut sm = IndexStateMachine::new();
        assert_eq!(sm.current(), IndexingPhase::Scanning);
        sm.advance();
        assert_eq!(sm.current(), IndexingPhase::Parsing);
    }
}
