#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dsolver_simulator::sim_analysis::run().await
}
