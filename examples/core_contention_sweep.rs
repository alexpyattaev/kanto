use std::{
    collections::HashMap,
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
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
    let timeout = tokio::time::timeout(Duration::from_secs(11), axum::serve(listener, app)).await;
    match timeout {
        Ok(v) => v.unwrap(),
        Err(_) => {
            println!("Terminating server on port {port}");
        }
    }
}
use kanto::*;
fn make_config_shared(cc: usize) -> RuntimeManagerConfig {
    let mut tokio_cfg_1 = TokioConfig::default();
    tokio_cfg_1.core_allocation = CoreAllocation::DedicatedCoreSet { min: 0, max: cc };
    tokio_cfg_1.worker_threads = cc;
    let mut tokio_cfg_2 = TokioConfig::default();
    tokio_cfg_2.core_allocation = CoreAllocation::DedicatedCoreSet { min: 0, max: cc };
    tokio_cfg_2.worker_threads = cc;
    RuntimeManagerConfig {
        tokio_configs: HashMap::from([
            ("tokio1".into(), tokio_cfg_1),
            ("tokio2".into(), tokio_cfg_2),
        ]),
        tokio_runtime_mapping: HashMap::from([
            ("axum1".into(), "tokio1".into()),
            ("axum2".into(), "tokio2".into()),
        ]),
        ..Default::default()
    }
}
fn make_config_dedicated(cc: usize) -> RuntimeManagerConfig {
    let mut tokio_cfg_1 = TokioConfig::default();
    tokio_cfg_1.core_allocation = CoreAllocation::DedicatedCoreSet {
        min: 0,
        max: cc / 2,
    };
    tokio_cfg_1.worker_threads = cc / 2;
    let mut tokio_cfg_2 = TokioConfig::default();
    tokio_cfg_2.core_allocation = CoreAllocation::DedicatedCoreSet {
        min: cc / 2,
        max: cc,
    };
    tokio_cfg_2.worker_threads = cc / 2;
    RuntimeManagerConfig {
        tokio_configs: HashMap::from([
            ("tokio1".into(), tokio_cfg_1),
            ("tokio2".into(), tokio_cfg_2),
        ]),
        tokio_runtime_mapping: HashMap::from([
            ("axum1".into(), "tokio1".into()),
            ("axum2".into(), "tokio2".into()),
        ]),
        ..Default::default()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Regime {
    Shared,
    Dedicated,
    Single,
}
impl Regime {
    const VALUES: [Self; 3] = [Self::Shared, Self::Dedicated, Self::Single];
}

#[derive(Debug, Default, serde::Serialize)]
struct Results {
    latencies_s: Vec<f32>,
    rps: Vec<f32>,
}

fn main() -> anyhow::Result<()> {
    let mut all_results: HashMap<String, Results> = HashMap::new();
    for regime in Regime::VALUES {
        let mut res = Results::default();
        for core_cnt in [2, 4, 8, 16] {
            let rtm;
            println!("===================");
            println!("Running {core_cnt} cores under {regime:?}");
            let (tok1, tok2) = match regime {
                Regime::Shared => {
                    rtm = RuntimeManager::new(make_config_shared(core_cnt)).unwrap();
                    (
                        rtm.get_tokio("axum1")
                            .expect("Expecting runtime named axum1"),
                        rtm.get_tokio("axum2")
                            .expect("Expecting runtime named axum2"),
                    )
                }
                Regime::Dedicated => {
                    rtm = RuntimeManager::new(make_config_dedicated(core_cnt)).unwrap();
                    (
                        rtm.get_tokio("axum1")
                            .expect("Expecting runtime named axum1"),
                        rtm.get_tokio("axum2")
                            .expect("Expecting runtime named axum2"),
                    )
                }
                Regime::Single => {
                    rtm = RuntimeManager::new(make_config_shared(core_cnt)).unwrap();
                    (
                        rtm.get_tokio("axum1")
                            .expect("Expecting runtime named axum1"),
                        rtm.get_tokio("axum2")
                            .expect("Expecting runtime named axum2"),
                    )
                }
            };

            let wrk_cores: Vec<_> = (32..64).collect();
            let results = std::thread::scope(|s| {
                s.spawn(|| {
                    tok1.start(axum_main(8888));
                });
                let jh = match regime {
                    Regime::Single => s.spawn(|| {
                        run_wrk(&[8888, 8888], &wrk_cores, wrk_cores.len(), 1000).unwrap()
                    }),
                    _ => {
                        s.spawn(|| {
                            tok2.start(axum_main(8889));
                        });
                        s.spawn(|| {
                            run_wrk(&[8888, 8889], &wrk_cores, wrk_cores.len(), 1000).unwrap()
                        })
                    }
                };
                jh.join().expect("WRK crashed!")
            });
            println!("Results are: {:?}", results);
            res.latencies_s.push(
                results.0.iter().map(|a| a.as_secs_f32()).sum::<f32>() / results.0.len() as f32,
            );
            res.rps.push(results.1.iter().sum());
        }
        all_results.insert(format!("{regime:?}"), res);
        std::thread::sleep(Duration::from_secs(3));
    }
    println!("{}", serde_json::to_string_pretty(&all_results)?);

    Ok(())
}

fn run_wrk(
    ports: &[u16],
    cpus: &[usize],
    threads: usize,
    connections: usize,
) -> anyhow::Result<(Vec<Duration>, Vec<f32>)> {
    let mut script = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    script.push("examples/report.lua");
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
                .arg(format!("-s{}", script.to_str().unwrap()))
                .arg(format!("-t{threads}"))
                .arg(format!("-c{connections}"))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();

    use std::str;
    let outs = children.drain(..).map(|c| c.wait_with_output().unwrap());
    let mut all_latencies = vec![];
    let mut all_rps = vec![];
    for (out, port) in outs.zip(ports.iter()) {
        println!("=========================");
        std::io::stdout().write_all(&out.stderr)?;
        let res = str::from_utf8(&out.stdout)?;
        let mut res = res.lines().last().unwrap().split(' ');

        let latency_us: u64 = res.next().unwrap().parse()?;
        let latency = Duration::from_micros(latency_us);

        let requests: usize = res.next().unwrap().parse()?;
        let rps = requests as f32 / 10.0;
        println!("WRK results for port {port}: {latency:?} {rps}");
        all_latencies.push(Duration::from_micros(latency_us));
        all_rps.push(rps);
    }
    Ok((all_latencies, all_rps))
}
