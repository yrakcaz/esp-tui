#[tokio::main]
async fn main() -> anyhow::Result<()> {
    esp_tui::app::run().await
}
