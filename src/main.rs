#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustpilot::app::run().await
}