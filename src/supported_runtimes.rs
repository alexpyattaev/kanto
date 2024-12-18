use crate::policy::CoreAllocation;
use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::Thread;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
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
#[serde(default)]
pub struct NativeConfig {
    pub core_allocation: CoreAllocation,
    pub max_threads: usize,
    pub priority: usize,
    pub name_base: String,
    pub stack_size_bytes: usize,
}
impl Default for NativeConfig {
    fn default() -> Self {
        Self {
            core_allocation: CoreAllocation::OsDefault,
            max_threads: 10,
            priority: 0,
            stack_size_bytes: 2 * 1024 * 1024,
            name_base: "thread".to_owned(),
        }
    }
}

#[derive(Debug)]
pub struct NativeThreadRuntime {
    pub id_count: AtomicUsize,
    pub running_count: Arc<AtomicUsize>,
    pub config: NativeConfig,
}

pub struct JoinHandle<T> {
    std_handle: Option<std::thread::JoinHandle<T>>,
    running_count: Arc<AtomicUsize>,
}

impl<T> JoinHandle<T> {
    fn join_inner(&mut self) -> Result<T, Box<dyn core::any::Any + Send + 'static>> {
        let r = match self.std_handle.take() {
            Some(jh) => {
                let r = jh.join();
                self.running_count.fetch_sub(1, Ordering::SeqCst);
                r
            }
            None => {
                panic!("Thread already joined");
            }
        };
        dbg!(self.std_handle.is_some());
        r
    }

    pub fn join(mut self) -> Result<T, Box<dyn core::any::Any + Send + 'static>> {
        self.join_inner()
    }

    pub fn is_finished(&self) -> bool {
        match self.std_handle {
            Some(ref jh) => jh.is_finished(),
            None => true,
        }
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if self.std_handle.is_some() {
            println!("Attempting to drop a Join Handle of a running thread will leak thread IDs, please join your managed threads!");
            self.join_inner().expect("Child thread panicked");
        }
    }
}

impl NativeThreadRuntime {
    pub fn new(cfg: NativeConfig) -> Self {
        Self {
            id_count: AtomicUsize::new(0),
            running_count: Arc::new(AtomicUsize::new(0)),
            config: cfg,
        }
    }
    pub fn spawn<F, T>(&self, f: F) -> anyhow::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Send + 'static,
    {
        let spawned = self.running_count.load(Ordering::SeqCst);
        if spawned >= self.config.max_threads {
            bail!("All allowed threads in this pool are already spawned");
        }
        let core_set: Vec<_> = match self.config.core_allocation {
            CoreAllocation::PinnedCores { min: _, max: _ } => {
                todo!("Need to store pinning mask somewhere");
            }
            CoreAllocation::DedicatedCoreSet { min, max } => (min..max).collect(),
            CoreAllocation::OsDefault => (0..affinity::get_core_num()).collect(),
        };

        let n = self.id_count.fetch_add(1, Ordering::SeqCst);
        let jh = std::thread::Builder::new()
            .name(format!("{}-{}", &self.config.name_base, n))
            .stack_size(self.config.stack_size_bytes)
            .spawn(move || {
                affinity::set_thread_affinity(core_set).expect("Can not set thread affinity");
                f()
            })?;
        self.running_count.fetch_add(1, Ordering::SeqCst);
        Ok(JoinHandle {
            std_handle: Some(jh),
            running_count: self.running_count.clone(),
        })
    }
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
        /*match self.config.core_allocation {
            CoreAllocation::PinnedCores { min: _, max: _ } => {
                todo!("NEed to store pinning mask somewhere");
            }
            CoreAllocation::DedicatedCoreSet { min, max } => {
                let mask: Vec<_> = (min..max).collect();
                println!("Constraining tokio main thread  to {:?}", &mask);
                affinity::set_thread_affinity(&mask)
                    .expect("Can not set thread affinity for runtime main thread");
            }
            CoreAllocation::OsDefault => {}
        }*/
        self.tokio.block_on(fut)
    }
}
