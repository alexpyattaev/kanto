use std::default;

use serde::{Deserialize, Serialize};

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub enum CoreAllocation {
    #[default]
    OsDefault,
    PinnedCores {
        min: usize,
        max: usize,
    },
    DedicatedCoreSet {
        min: usize,
        max: usize,
    },
}
