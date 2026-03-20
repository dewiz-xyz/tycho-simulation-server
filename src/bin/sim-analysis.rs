#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tycho_simulation_server::sim_analysis::run().await
}
