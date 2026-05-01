mod app;
mod demo;
mod elf;
mod event;
mod filter;
mod flash;
mod log;
mod port;
mod serial;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
