use super::*;

/// Represents a strategy that can be run in an [`Arena`].
pub trait Strategy {
    /// Initialization function for ths strategy to be run upon simulation startup.
    fn init(&self, provider: AnvilProvider, signal: Signal);

    /// Processing function for the strategy to be run each simulation step.
    fn process(&self, provider: AnvilProvider, signal: Signal);
}
