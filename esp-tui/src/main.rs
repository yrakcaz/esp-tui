mod app;
mod demo;
mod event;
mod filter;
mod log;
mod serial;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
