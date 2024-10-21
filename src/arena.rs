use std::{collections::HashMap, time::Instant};

use alloy::{
    providers::{Provider, ProviderBuilder, WalletProvider},
    signers::local::PrivateKeySigner,
};

use super::*;
use crate::{
    config::Config,
    engine::{arbitrageur::Arbitrageur, inspector::Inspector},
    error::ArenaError,
    feed::Feed,
    strategy::Strategy,
    types::controller::ArenaController,
};

/// Represents an [`Arena`] that can be used to run a simulation and execute strategies.
pub struct Arena<V> {
    /// The underlying Anvil execution environment.
    pub env: AnvilInstance,

    /// The strategies that are to be run in the simulation.
    pub strategies: Vec<Box<dyn Strategy<V>>>,

    /// The feed that provides the current, theoretical value of the pool.
    pub feed: Box<dyn Feed>,

    /// The inspector that is used to evaluate the performance of the strategies.
    pub inspector: Box<dyn Inspector<V>>,

    /// The arbitrageur that is used to peg the pool.
    pub arbitrageur: Box<dyn Arbitrageur>,

    providers: HashMap<usize, AnvilProvider>,
}

#[allow(clippy::redundant_closure)]
impl<V> Arena<V> {
    /// Run all strategies in the simulation with a given configuration.
    pub async fn run(&mut self, config: Config) -> Result<(), ArenaError> {
        let admin_provider = self.providers[&0].clone();

        let controller = ArenaController::deploy(
            admin_provider.clone(),
            config.manager_fee,
            config.initial_price,
        )
        .await?;

        controller
            .setPool(
                config.pool_fee,
                config.tick_spacing,
                config.hooks,
                config.sqrt_price_x96,
                config.hook_data,
            )
            .send()
            .await
            .map_err(ArenaError::ContractError)?
            .watch()
            .await
            .map_err(|e| ArenaError::PendingTransactionError(e))?;

        let engine = Engine {
            controller: *controller.address(),
        };

        for (idx, strategy) in self.strategies.iter_mut().enumerate() {
            let strategy_provider = self.providers[&(idx + 1)].clone();

            let signal = controller.constructSignal().call().await?._0;

            let signal = Signal::new(
                signal.lexPrice,
                None,
                signal.currentTick,
                signal.sqrtPriceX96,
                signal.manager,
                signal.pool,
                signal.fetcher,
                self.feed.current_value(),
                *controller.address(),
            );

            strategy
                .init(
                    strategy_provider.clone(),
                    signal,
                    &mut self.inspector,
                    engine.clone(),
                )
                .await;
        }

        let signal = controller.constructSignal().call().await?._0;

        let signal = Signal::new(
            signal.lexPrice,
            None,
            signal.currentTick,
            signal.sqrtPriceX96,
            signal.manager,
            signal.pool,
            signal.fetcher,
            self.feed.current_value(),
            *controller.address(),
        );

        self.arbitrageur.init(&signal, admin_provider.clone()).await;

        for step in 0..config.steps {
            let instant = Instant::now();
            controller
                .setPrice(
                    alloy::primitives::utils::parse_ether(&self.feed.step().to_string())
                        .map_err(ArenaError::ConversionError)?,
                )
                .nonce(
                    admin_provider
                        .get_transaction_count(admin_provider.default_signer_address())
                        .await
                        .unwrap(),
                )
                .send()
                .await
                .map_err(ArenaError::ContractError)?
                .watch()
                .await
                .map_err(|e| ArenaError::PendingTransactionError(e))?;

            let signal = controller.constructSignal().call().await?._0;

            let signal = Signal::new(
                signal.lexPrice,
                None,
                signal.currentTick,
                signal.sqrtPriceX96,
                signal.manager,
                signal.pool,
                signal.fetcher,
                self.feed.current_value(),
                *controller.address(),
            );

            self.arbitrageur
                .arbitrage(&signal, admin_provider.clone())
                .await;

            for (idx, strategy) in self.strategies.iter_mut().enumerate() {
                let signal = controller.constructSignal().call().await?._0;

                let signal = Signal::new(
                    signal.lexPrice,
                    Some(step),
                    signal.currentTick,
                    signal.sqrtPriceX96,
                    signal.manager,
                    signal.pool,
                    signal.fetcher,
                    self.feed.current_value(),
                    *controller.address(),
                );

                strategy
                    .process(
                        self.providers[&(idx + 1)].clone(),
                        signal,
                        &mut self.inspector,
                        engine.clone(),
                    )
                    .await;
            }

            self.feed.step();
            println!("Step {} took {:?}", step, instant.elapsed());
        }

        // controller
        //     .addLiquidity(1000)
        //     .send()
        //     .await
        //     .map_err(ArenaError::ContractError)?
        //     .watch()
        //     .await
        //     .map_err(|e| ArenaError::PendingTransactionError(e))?;

        Ok(())
    }
}

/// A builder for an [`Arena`] that can be used to configure the simulation.
pub struct ArenaBuilder<V> {
    /// [`Arena::env`]
    pub env: AnvilInstance,

    /// [`Arena::strategies`]
    pub strategies: Vec<Box<dyn Strategy<V>>>,

    /// [`Arena::feed`]
    pub feed: Option<Box<dyn Feed>>,

    /// [`Arena::inspector`]
    pub inspector: Option<Box<dyn Inspector<V>>>,

    /// [`Arena::arbitrageur`]
    pub arbitrageur: Option<Box<dyn Arbitrageur>>,
}

impl<V> Default for ArenaBuilder<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> ArenaBuilder<V> {
    /// Public constructor function for a new [`ArenaBuilder`].
    pub fn new() -> Self {
        ArenaBuilder {
            env: Anvil::default().spawn(),
            strategies: Vec::new(),
            feed: None,
            inspector: None,
            arbitrageur: None,
        }
    }

    /// Add a strategy to the simulation.
    pub fn with_strategy(mut self, strategy: Box<dyn Strategy<V>>) -> Self {
        self.strategies.push(strategy);
        self
    }

    /// Set the feed that provides the current, theoretical value of the pool.
    pub fn with_feed(mut self, feed: Box<dyn Feed>) -> Self {
        self.feed = Some(feed);
        self
    }

    /// Set the inspector that is used to evaluate the performance of the strategies.
    pub fn with_inspector(mut self, inspector: Box<dyn Inspector<V>>) -> Self {
        self.inspector = Some(inspector);
        self
    }

    /// Set the inspector that is used to evaluate the performance of the strategies.
    pub fn with_arbitrageur(mut self, arbitrageur: Box<dyn Arbitrageur>) -> Self {
        self.arbitrageur = Some(arbitrageur);
        self
    }

    /// Build the [`Arena`] with the given configuration.
    pub fn build(self) -> Arena<V> {
        let mut providers = HashMap::new();

        for i in 0..9 {
            let signer: PrivateKeySigner = self.env.keys()[i].clone().into();
            let wallet = EthereumWallet::from(signer);

            let rpc_url = self.env.endpoint().parse().unwrap();

            let provider = ProviderBuilder::new()
                .with_recommended_fillers()
                .wallet(wallet)
                .on_http(rpc_url);

            providers.insert(i, provider);
        }

        Arena {
            env: self.env,
            strategies: self.strategies,
            feed: self.feed.unwrap(),
            inspector: self.inspector.unwrap(),
            arbitrageur: self.arbitrageur.unwrap(),
            providers,
        }
    }
}
