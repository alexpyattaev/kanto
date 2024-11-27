use affinity::*;
use anyhow::Ok;
use serde::{Deserialize, Serialize};
use thread_priority::*;

pub type ConstString = Box<str>;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

mod policy;
mod supported_runtimes;
pub use policy::CoreAllocation;
pub use supported_runtimes::{NativeConfig, NativeThreadRuntime, TokioConfig, TokioRuntime};

#[derive(Default, Debug)]
pub struct RuntimeManager {
    pub tokio_runtimes: HashMap<ConstString, TokioRuntime>,
    pub tokio_runtime_mapping: HashMap<ConstString, ConstString>,

    pub native_thread_runtimes: HashMap<ConstString, NativeThreadRuntime>,
    pub native_runtime_mapping: HashMap<ConstString, ConstString>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeManagerConfig {
    pub tokio_configs: HashMap<String, TokioConfig>,
    pub tokio_runtime_mapping: HashMap<String, String>,
    pub native_runtime_mapping: HashMap<String, String>,
    pub native_configs: HashMap<String, NativeConfig>,
    pub default_core_allocation: CoreAllocation,
}

impl RuntimeManager {
    pub fn get_native(&self, name: &str) -> Option<&NativeThreadRuntime> {
        let n = self.native_runtime_mapping.get(name)?;
        self.native_thread_runtimes.get(n)
    }
    pub fn get_tokio(&self, name: &str) -> Option<&TokioRuntime> {
        let n = self.tokio_runtime_mapping.get(name)?;
        self.tokio_runtimes.get(n)
    }
    pub fn set_process_affinity(config: &RuntimeManagerConfig) -> anyhow::Result<Vec<usize>> {
        let chosen_cores_mask: Vec<usize> = {
            match config.default_core_allocation {
                CoreAllocation::PinnedCores { min, max } => (min..max).collect(),
                CoreAllocation::DedicatedCoreSet { min, max } => (min..max).collect(),
                CoreAllocation::OsDefault => vec![],
            }
        };

        match set_thread_affinity(&chosen_cores_mask) {
            Err(e) => anyhow::bail!(e.to_string()),
            _ => {}
        }
        Ok(chosen_cores_mask)
    }

    pub fn new(config: RuntimeManagerConfig) -> anyhow::Result<Self> {
        let mut core_allocations = HashMap::<ConstString, Vec<usize>>::new();
        Self::set_process_affinity(&config)?;
        let mut manager = Self::default();

        //TODO: this should probably be cleaned up at some point...
        for (k, v) in config.tokio_runtime_mapping.iter() {
            manager
                .tokio_runtime_mapping
                .insert(k.clone().into_boxed_str(), v.clone().into_boxed_str());
        }
        for (k, v) in config.native_runtime_mapping.iter() {
            manager
                .native_runtime_mapping
                .insert(k.clone().into_boxed_str(), v.clone().into_boxed_str());
        }

        for (name, cfg) in config.native_configs.iter() {
            let nrt = NativeThreadRuntime::new(cfg.clone());
            manager
                .native_thread_runtimes
                .insert(name.clone().into_boxed_str(), nrt);
        }

        for (name, cfg) in config.tokio_configs.iter() {
            let num_workers = if cfg.worker_threads != 0 {
                cfg.worker_threads
            } else {
                get_core_num()
            };

            // keep track of cores allocated for this runtime
            let chosen_cores_mask: Vec<usize> = {
                match cfg.core_allocation {
                    CoreAllocation::PinnedCores { min, max } => (min..max).collect(),
                    CoreAllocation::DedicatedCoreSet { min, max } => (min..max).collect(),
                    CoreAllocation::OsDefault => vec![],
                }
            };
            core_allocations.insert(name.clone().into_boxed_str(), chosen_cores_mask.clone());

            let base_name = name.clone();
            println!(
                "Assigning {:?} to runtime {}",
                &chosen_cores_mask, &base_name
            );
            let mut builder = match num_workers {
                1 => tokio::runtime::Builder::new_current_thread(),

                _ => {
                    let mut builder = tokio::runtime::Builder::new_multi_thread();
                    builder.worker_threads(num_workers);
                    builder
                }
            };
            let atomic_id: AtomicUsize = AtomicUsize::new(0);
            builder
                .event_interval(cfg.event_interval)
                .thread_name_fn(move || {
                    let id = atomic_id.fetch_add(1, Ordering::SeqCst);
                    format!("{}-{}", base_name, id)
                })
                .thread_stack_size(cfg.stack_size_bytes)
                .enable_all()
                .max_blocking_threads(cfg.max_blocking_threads);

            //keep borrow checker happy and move these things into the closure
            let c = cfg.clone();
            let chosen_cores_mask = Mutex::new(chosen_cores_mask);
            builder.on_thread_start(move || {
                let cur_thread = std::thread::current();
                let _tid = cur_thread
                    .get_native_id()
                    .expect("Can not get thread id for newly created thread");
                let tname = cur_thread.name().unwrap();
                //println!("thread {tname} id {tid} started");
                std::thread::current()
                    .set_priority(thread_priority::ThreadPriority::Crossplatform(
                        (c.priority as u8).try_into().unwrap(),
                    ))
                    .expect("Can not set thread priority!");

                match c.core_allocation {
                    CoreAllocation::PinnedCores { min: _, max: _ } => {
                        let mut lg = chosen_cores_mask
                            .lock()
                            .expect("Can not lock core mask mutex");
                        let core = lg
                            .pop()
                            .expect("Not enough cores provided for pinned allocation");
                        println!("Pinning worker {tname} to core {core}");
                        set_thread_affinity(&[core])
                            .expect("Can not set thread affinity for runtime worker");
                    }
                    CoreAllocation::DedicatedCoreSet { min: _, max: _ } => {
                        let lg = chosen_cores_mask
                            .lock()
                            .expect("Can not lock core mask mutex");
                        set_thread_affinity(&(*lg))
                            .expect("Can not set thread affinity for runtime worker");
                    }
                    CoreAllocation::OsDefault => {}
                }
            });
            manager.tokio_runtimes.insert(
                name.clone().into_boxed_str(),
                TokioRuntime {
                    tokio: builder.build()?,
                    config: cfg.clone(),
                },
            );
        }
        Ok(manager)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::{CoreAllocation, NativeConfig, RuntimeManager, RuntimeManagerConfig};

    #[test]
    fn process_affinity() {
        let conf = RuntimeManagerConfig {
            native_configs: HashMap::from([(
                "pool1".to_owned(),
                NativeConfig {
                    core_allocation: CoreAllocation::DedicatedCoreSet { min: 0, max: 4 },
                    max_threads: 5,
                    priority: 0,
                    ..Default::default()
                },
            )]),
            default_core_allocation: CoreAllocation::DedicatedCoreSet { min: 4, max: 8 },
            native_runtime_mapping: HashMap::from([("test".to_owned(), "pool1".to_owned())]),
            ..Default::default()
        };

        let rtm = RuntimeManager::new(conf).unwrap();
        let r = rtm.get_native("test").unwrap();

        let t2 = r
            .spawn(|| {
                let aff = affinity::get_thread_affinity().unwrap();
                assert_eq!(aff, [0, 1, 2, 3], "Managed thread allocation should be 0-3");
            })
            .unwrap();

        let t = std::thread::spawn(|| {
            let aff = affinity::get_thread_affinity().unwrap();
            assert_eq!(aff, [4, 5, 6, 7], "Default thread allocation should be 4-7");

            let tt = std::thread::spawn(|| {
                let aff = affinity::get_thread_affinity().unwrap();
                assert_eq!(
                    aff,
                    [4, 5, 6, 7],
                    "Nested thread allocation should still be 4-7"
                );
            });
            tt.join().unwrap();
        });
        t.join().unwrap();
        t2.join().unwrap();
    }
}
