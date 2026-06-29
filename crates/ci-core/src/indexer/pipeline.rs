use crate::types::IndexingPhase;
use rusqlite::Connection;

pub fn run_indexing_pipeline(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    // Insert symbols...
    // Build edges...
    // Call coreness & update_is_hub_flags...
    
    // Only commit if ALL steps succeed, avoiding corrupted intermediate graph states
    tx.commit()?;
    Ok(())
}

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

    #[test]
    fn test_run_indexing_pipeline_transaction() {
        use crate::db::schema::init_db;
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        
        let result = run_indexing_pipeline(&mut conn);
        assert!(result.is_ok());
    }
}
