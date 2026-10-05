use futures::future::BoxFuture;

pub trait Runner: Send + Sync {
    /// Start running the runner in background.
    fn run_async(&self);
    /// Wait for initialization; runners without a readiness signal return immediately.
    fn wait_ready(&self) -> BoxFuture<'_, Result<(), crate::Error>> {
        Box::pin(async { Ok(()) })
    }
    /// Signal the runner to shutdown.
    fn shutdown(&self);
    /// Wait for the runner to finish.
    #[allow(dead_code)]
    fn join(&self) -> BoxFuture<'_, Result<(), crate::Error>>;
}
