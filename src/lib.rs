use affinity::*;
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
use policy::CoreAllocation;
use supported_runtimes::{NativeConfig, NativeThreadRuntime, TokioConfig, TokioRuntime};

static DBG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Default, Debug)]
pub struct RuntimeManager {
    pub tokio_runtimes: HashMap<ConstString, TokioRuntime>,
    pub tokio_runtime_mapping: HashMap<ConstString, ConstString>,
    pub native_thread_runtimes: HashMap<ConstString, NativeThreadRuntime>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeManagerConfig {
    pub tokio_configs: HashMap<String, TokioConfig>,
    pub tokio_runtime_mapping: HashMap<String, String>,
    pub native_configs: HashMap<String, NativeConfig>,
}

impl RuntimeManager {
    pub fn get_tokio(&self, name: &str) -> Option<&TokioRuntime> {
        let n = self.tokio_runtime_mapping.get(name)?;
        self.tokio_runtimes.get(n)
    }
    pub fn new(config: RuntimeManagerConfig) -> anyhow::Result<Self> {
        let mut core_allocations = HashMap::<ConstString, Vec<usize>>::new();
        let mut manager = Self::default();
        for (k, v) in config.tokio_runtime_mapping.iter() {
            manager
                .tokio_runtime_mapping
                .insert(k.clone().into_boxed_str(), v.clone().into_boxed_str());
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
                &core_allocations, &base_name
            );
            let mut builder = match num_workers {
                1 => tokio::runtime::Builder::new_current_thread(),

                _ => {
                    let mut builder = tokio::runtime::Builder::new_multi_thread();
                    builder.worker_threads(num_workers - 1);
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
                println!("==================");

                let _lg = DBG_LOCK.lock();
                let cur_thread = std::thread::current();
                let tid = cur_thread
                    .get_native_id()
                    .expect("Can not get thread id for newly created thread");
                let tname = cur_thread.name().unwrap();
                println!("thread {tname} id {tid} started");
                let priority = std::thread::current()
                    .get_priority()
                    .expect("Can not get priority");
                //println!("current priority is {priority:?}");
                //println!(
                //    "\tCurrent thread affinity : {:?}",
                //    get_thread_affinity().unwrap()
                //);

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
                println!("==================");
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
    use std::{
        collections::HashMap,
        io::Write,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::Duration,
    };

    async fn axum_main(port: u16) {
        use axum::{routing::get, Router};

        // basic handler that responds with a static string
        async fn root() -> &'static str {
            tokio::time::sleep(Duration::from_millis(1)).await;
            "Hello, World!"
        }

        // build our application with a route
        let app = Router::new().route("/", get(root));

        // run our app with hyper, listening globally on port 3000
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))
                .await
                .unwrap();
        let timeout =
            tokio::time::timeout(Duration::from_secs(11), axum::serve(listener, app)).await;
        match timeout {
            Ok(v) => v.unwrap(),
            Err(_) => {
                println!("Terminating server on port {port}");
            }
        }
    }
    use super::*;
    #[test]
    fn it_works() {
        {
            let _lg = DBG_LOCK.lock();
            println!(
                "\tCurrent thread affinity : {:?}",
                get_thread_affinity().unwrap()
            );
            println!("\tTotal cores : {}", get_core_num());
        }

        let mut tokio_cfg_1 = TokioConfig::default();
        tokio_cfg_1.core_allocation = CoreAllocation::DedicatedCoreSet { min: 0, max: 4 };
        tokio_cfg_1.worker_threads = 4;
        let mut tokio_cfg_2 = TokioConfig::default();
        tokio_cfg_2.core_allocation = CoreAllocation::DedicatedCoreSet { min: 4, max: 8 };
        tokio_cfg_2.worker_threads = 5;
        /*
        let mut tokio_cfg_1 = TokioConfig::default();
        tokio_cfg_1.core_allocation = CoreAllocation::DedicatedCoreSet { min: 0, max: 8 };
        tokio_cfg_1.worker_threads = 8;
        let mut tokio_cfg_2 = TokioConfig::default();
        tokio_cfg_2.core_allocation = CoreAllocation::DedicatedCoreSet { min: 0, max: 8 };
        tokio_cfg_2.worker_threads = 8;
        */
        let cfg = RuntimeManagerConfig {
            tokio_configs: HashMap::from([
                ("tokio1".into(), tokio_cfg_1),
                ("tokio2".into(), tokio_cfg_2),
            ]),
            tokio_runtime_mapping: HashMap::from([
                ("axum1".into(), "tokio1".into()),
                ("axum2".into(), "tokio2".into()),
            ]),
            native_configs: HashMap::new(),
        };

        let rtm = RuntimeManager::new(cfg).unwrap();
        {
            let _lg = DBG_LOCK.lock();
            dbg!(&rtm.tokio_runtime_mapping);
            dbg!(&rtm.tokio_runtimes);
        }
        let tok1 = rtm.get_tokio("axum1").unwrap();
        let tok2 = rtm.get_tokio("axum2").unwrap();
        let wrk_cores: Vec<_> = (32..64).collect();
        std::thread::scope(|s| {
            s.spawn(|| {
                tok1.start(axum_main(8888));
            });
            s.spawn(|| {
                tok2.start(axum_main(8889));
            });
            s.spawn(|| {
                run_wrk(&[8888, 8889], &wrk_cores, wrk_cores.len(), 1000).unwrap();
            });
        });
    }

    fn run_wrk(
        ports: &[u16],
        cpus: &[usize],
        threads: usize,
        connections: usize,
    ) -> anyhow::Result<()> {
        let cpus: Vec<String> = cpus.iter().map(|c| c.to_string()).collect();
        let cpus = cpus.join(",");

        let mut children: Vec<_> = ports
            .iter()
            .map(|p| {
                std::process::Command::new("taskset")
                    .arg("-c")
                    .arg(&cpus)
                    .arg("wrk")
                    .arg(format!("http://localhost:{}", p))
                    .arg("-d10")
                    .arg(format!("-t{threads}"))
                    .arg(format!("-c{connections}"))
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();

        let outs = children.drain(..).map(|c| c.wait_with_output().unwrap());
        for (out, port) in outs.zip(ports.iter()) {
            println!("=========================");
            println!("WRK results for port {port}");
            std::io::stdout().write_all(&out.stderr)?;
            std::io::stdout().write_all(&out.stdout)?;
        }
        Ok(())
    }
}
