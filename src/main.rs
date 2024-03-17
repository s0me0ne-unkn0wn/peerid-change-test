use clap::Parser;
use subxt::rpc_params;
use zombienet_sdk::{NetworkConfigBuilder, NetworkConfigExt, NetworkNode};

use core::time::Duration;

struct HostnameGen {
    prefix: String,
    count: usize,
}

impl HostnameGen {
    fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            count: 0,
        }
    }

    fn next(&mut self) -> String {
        self.count += 1;
        format!("{}{:02}", self.prefix, self.count)
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Number of relay chain validators
    #[arg(long, short, default_value_t = 3_usize)]
    relay: usize,

    /// Block height to wait for before replacing the node PeerId
    #[arg(long, short, default_value_t = 4_usize)]
    block_height: usize,
}

async fn wait_for_metric(
    node: &NetworkNode,
    metric: impl Into<String> + Copy,
    timeout: Duration,
    predicate: impl Fn(f64) -> bool + Copy,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tokio::time::timeout(timeout, async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            log::trace!("Checking metric {}", metric.into());
            match node.assert_with(metric, predicate).await {
                Ok(r) => {
                    if r {
                        return Ok(());
                    }
                }
                Err(e) => {
                    let cause = e.to_string();
                    if let Ok(ioerr) = e.downcast::<std::io::Error>() {
                        if ioerr.kind() == std::io::ErrorKind::ConnectionRefused {
                            log::warn!("Ignoring connection refused error");
                            // The node is not ready to accept connections yet
                            continue;
                        }
                    }
                    panic!("Cannot assert on node metric: {:?}", cause)
                }
            }
        }
    })
    .await?
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );

    let args = Args::parse();
    let mut relay_hostname = HostnameGen::new("validator");

    let relay = NetworkConfigBuilder::new().with_relaychain(|r| {
        let mut r = r
            .with_chain("rococo-local")
            .with_default_command("polkadot")
            .with_node(|node| node.with_name(relay_hostname.next().as_str()));
        for _ in 1..args.relay {
            r = r.with_node(|node| node.with_name(relay_hostname.next().as_str()));
        }
        r
    });

    // TODO: Add parachains here, if needed

    let network = relay.build().unwrap();
    let mut network = network.spawn_native().await?;

    // The first node becomes a bootnode so we're performing all the cruel surgery on the second one
    let node = network.get_node("validator02")?;
    // This is just to wait when the node is RPC-ready
    wait_for_metric(
        node,
        "block_height{status=\"best\"}",
        Duration::from_secs(300),
        |v| v >= 1.0,
    )
    .await?;

    let peer_id: String = node
        .rpc()
        .await?
        .request("system_localPeerId", rpc_params![])
        .await?;

    log::info!("Initial PeerId: {:?}", peer_id);

    log::info!(
        "Waiting for the node to reach the desired block height of {}",
        args.block_height
    );

    wait_for_metric(
        node,
        "block_height{status=\"best\"}",
        Duration::from_secs(300),
        |v| v >= args.block_height as f64,
    )
    .await?;

    let mut spec = node.spec().clone();
    node.kill().await?;

    log::info!("Node killed, replacing it...");

    (spec.key, spec.peer_id) =
        zombienet_orchestrator::generators::generate_node_identity("new-key-seed")?;
    network.replace_node(spec).await?;

    let node = network.get_node("validator02")?;
    // This is just to wait when the node is RPC-ready
    wait_for_metric(
        node,
        "block_height{status=\"best\"}",
        Duration::from_secs(300),
        |v| v >= args.block_height as f64,
    )
    .await?;

    let peer_id: String = node
        .rpc()
        .await?
        .request("system_localPeerId", rpc_params![])
        .await?;

    log::info!("New PeerId: {:?}", peer_id);

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
