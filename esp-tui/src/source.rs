/// Abstraction over anything that produces serial log lines.
///
/// Implementors spawn a background task that sends [`crate::event::Message::Serial`]
/// lines through `tx` until the `shutdown` watch is set to `true`.
pub trait Emitter: Send + 'static {
    /// Spawns the background producer task.
    ///
    /// # Arguments
    ///
    /// * `tx` - Unbounded sender for forwarding log line events to the event loop.
    /// * `shutdown` - Watch receiver; the task should exit when the value
    ///   becomes `true`.
    ///
    /// # Returns
    ///
    /// A [`tokio::task::JoinHandle`] for the spawned task.
    fn spawn(
        self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::event::Message>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()>;
}
