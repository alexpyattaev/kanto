use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreAllocation {
    PinnedCores { min: usize, max: usize },
    DedicatedCoreSet { min: usize, max: usize },
    OsDefault,
}
