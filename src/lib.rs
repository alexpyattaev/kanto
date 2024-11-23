use affinity::*;
use serde::{Deserialize, Serialize};
use thread_priority::*;

pub type ConstString = Box<str>;
use std::{
    collections::HashMap,
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
    thread::Thread,
};

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
static DBG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Default, Debug)]
pub struct RuntimeManager {
    pub tokio_runtimes: HashMap<ConstString, TokioRuntime>,
    pub tokio_runtime_mapping: HashMap<ConstString, ConstString>,
    pub native_thread_runtimes: HashMap<ConstString, NativeThreadRuntime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreAllocation {
    PinnedCores { min: usize, max: usize },
    DedicatedCoreSet { min: usize, max: usize },
    OsDefault,
}

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
        let core_allocations = HashMap::<ConstString, Vec<usize>>::new();
        let mut manager = Self::default();
        for (k, v) in config.tokio_runtime_mapping.iter() {
            manager
                .tokio_runtime_mapping
                .insert(k.clone().into_boxed_str(), v.clone().into_boxed_str());
        }

        for (name, c) in config.tokio_configs.iter() {
            let num_workers = if c.worker_threads != 0 {
                c.worker_threads
            } else {
                get_core_num()
            };
            let base_name = name.clone();
            let mut builder = match num_workers {
                1 => tokio::runtime::Builder::new_current_thread(),

                _ => {
                    let mut builder = tokio::runtime::Builder::new_multi_thread();
                    builder.worker_threads(num_workers - 1);
                    builder
                }
            };
            builder
                .event_interval(c.event_interval)
                .thread_name_fn(move || {
                    static ATOMIC_ID: AtomicUsize = AtomicUsize::new(0);
                    let id = ATOMIC_ID.fetch_add(1, Ordering::SeqCst);
                    format!("{}-{}", base_name, id)
                })
                .thread_stack_size(c.stack_size_bytes)
                .enable_all()
                .max_blocking_threads(c.max_blocking_threads);

            let base_name = name.clone();
            builder
                .on_thread_start(move || {
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
                    println!("current priority is {priority:?}");
                    println!(
                        "\tCurrent thread affinity : {:?}",
                        get_thread_affinity().unwrap()
                    );
                    println!("==================");
                })
                .thread_stack_size(c.stack_size_bytes);
            manager.tokio_runtimes.insert(
                name.clone().into_boxed_str(),
                TokioRuntime {
                    tokio: builder.build()?,
                    config: c.clone(),
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
    };

    async fn axum_main(port: u16) {
        use axum::{
            http::StatusCode,
            routing::{get, post},
            Json, Router,
        };

        // basic handler that responds with a static string
        async fn root() -> &'static str {
            "Hello, World!"
        }

        // build our application with a route
        let app = Router::new().route("/", get(root));

        // run our app with hyper, listening globally on port 3000
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))
                .await
                .unwrap();
        axum::serve(listener, app).await.unwrap();
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
        tokio_cfg_1.core_allocation = CoreAllocation::DedicatedCoreSet { min: 0, max: 2 };
        tokio_cfg_1.worker_threads = 1;
        let mut tokio_cfg_2 = TokioConfig::default();
        tokio_cfg_2.core_allocation = CoreAllocation::DedicatedCoreSet { min: 3, max: 5 };
        tokio_cfg_1.worker_threads = 3;

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
        tok1.start(axum_main(8888));
    }

    fn run_wrk(ports: Vec<u16>) -> anyhow::Result<()> {
        let mut children: Vec<_> = ports
            .iter()
            .map(|p| {
                std::process::Command::new("wrk")
                    .arg(format!("http://localhost:{}", p))
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();

        let outs = children.drain(..).map(|c| c.wait_with_output().unwrap());
        for out in outs {
            std::io::stdout().write_all(&out.stderr)?;
            std::io::stdout().write_all(&out.stdout)?;
        }
        Ok(())
    }
}
