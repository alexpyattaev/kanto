use crate::policy::CoreAllocation;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::thread::Thread;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokioConfig {
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
    pub priority: u32,
    pub stack_size_bytes: usize,
    pub event_interval: u32,
    pub core_allocation: CoreAllocation,
}

impl Default for TokioConfig {
    fn default() -> Self {
        Self {
            core_allocation: CoreAllocation::OsDefault,
            worker_threads: 1,
            max_blocking_threads: 1,
            priority: 0,
            stack_size_bytes: 2 * 1024 * 1024,
            event_interval: 61,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeConfig {
    pub max_threads: usize,
    pub priority: usize,
}

#[derive(Debug)]
pub struct NativeThreadRuntime {
    pub threads: Vec<Thread>,
    pub config: NativeConfig,
}

#[derive(Debug)]
pub struct TokioRuntime {
    pub(crate) tokio: tokio::runtime::Runtime,
    pub config: TokioConfig,
}
impl TokioRuntime {
    /* This is bad idea...
    pub fn spawn<F>(&self, fut: F)-><F as Future>::Output
    where F: Future
    {
        self.tokio.spawn(fut)
    }
    pub fn spawn_blocking<F>(&self, fut: F)-><F as Future>::Output
    where F: Future
    {
        self.spawn(fut)
    }
    */
    pub fn start<F>(&self, fut: F) -> F::Output
    where
        F: Future,
    {
        //assign_core(self.config.core_allocation, self.config.priority);
        self.tokio.block_on(fut)
    }
}
