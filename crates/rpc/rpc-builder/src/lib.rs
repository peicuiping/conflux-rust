mod constants;
mod error;
mod id_provider;
mod module;
mod result;

pub use error::*;
pub use id_provider::EthSubscriptionIdProvider;
pub use module::{EthRpcModule, RpcModuleSelection};
pub use result::*;
use serde::{Deserialize, Serialize};

use cfx_rpc::{helpers::ChainInfo, *};
use cfx_rpc_cfx_types::RpcImplConfiguration;
use cfx_rpc_eth_api::*;
use cfx_types::U256;
use cfxcore::{
    SharedConsensusGraph, SharedSynchronizationService, SharedTransactionPool,
};
pub use jsonrpsee::server::ServerBuilder;
use jsonrpsee::{
    core::RegisterMethodError,
    server::{
        // middleware::rpc::{RpcService, RpcServiceT},
        AlreadyStoppedError,
        IdProvider,
        RpcServiceBuilder,
        ServerHandle,
    },
    Methods, RpcModule,
};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    // time::{Duration, SystemTime, UNIX_EPOCH},
};
pub use tower::layer::util::{Identity, Stack};
// use tower::Layer;

/// A builder type to configure the RPC module: See [`RpcModule`]
///
/// This is the main entrypoint and the easiest way to configure an RPC server.
#[derive(Clone)]
pub struct RpcModuleBuilder {
    config: RpcImplConfiguration,
    consensus: SharedConsensusGraph,
    sync: SharedSynchronizationService,
    tx_pool: SharedTransactionPool,
    max_estimation_gas_limit: Option<U256>,
}

impl RpcModuleBuilder {
    pub fn new(
        config: RpcImplConfiguration, consensus: SharedConsensusGraph,
        sync: SharedSynchronizationService, tx_pool: SharedTransactionPool,
        max_estimation_gas_limit: Option<U256>,
    ) -> Self {
        Self {
            config,
            consensus,
            sync,
            tx_pool,
            max_estimation_gas_limit,
        }
    }

    /// Configures all [`RpcModule`]s specific to the given
    /// [`TransportRpcModuleConfig`] which can be used to start the
    /// transport server(s).
    pub fn build(
        self, module_config: TransportRpcModuleConfig,
    ) -> TransportRpcModules<()> {
        let mut modules = TransportRpcModules::default();

        if !module_config.is_empty() {
            let TransportRpcModuleConfig {
                http,
                ws,
                config: _,
            } = module_config.clone();

            let Self {
                config: rpc_config,
                consensus,
                sync,
                tx_pool,
                max_estimation_gas_limit,
            } = self;

            let mut registry = RpcRegistryInner::new(
                rpc_config,
                consensus,
                sync,
                tx_pool,
                max_estimation_gas_limit,
            );

            modules.config = module_config;
            modules.http = registry.maybe_module(http.as_ref());
            modules.ws = registry.maybe_module(ws.as_ref());
        }

        modules
    }
}

/// A Helper type the holds instances of the configured modules.
#[derive(Clone)]
pub struct RpcRegistryInner {
    consensus: SharedConsensusGraph,
    max_estimation_gas_limit: Option<U256>,
    modules: HashMap<EthRpcModule, Methods>,
    config: RpcImplConfiguration,
    sync: SharedSynchronizationService,
    tx_pool: SharedTransactionPool,
}

impl RpcRegistryInner {
    pub fn new(
        config: RpcImplConfiguration, consensus: SharedConsensusGraph,
        sync: SharedSynchronizationService, tx_pool: SharedTransactionPool,
        max_estimation_gas_limit: Option<U256>,
    ) -> Self {
        Self {
            consensus,
            max_estimation_gas_limit,
            config,
            sync,
            tx_pool,
            modules: Default::default(),
        }
    }

    /// Returns all installed methods
    pub fn methods(&self) -> Vec<Methods> {
        self.modules.values().cloned().collect()
    }

    /// Returns a merged `RpcModule`
    pub fn module(&self) -> RpcModule<()> {
        let mut module = RpcModule::new(());
        for methods in self.modules.values().cloned() {
            module.merge(methods).expect("No conflicts");
        }
        module
    }
}

impl RpcRegistryInner {
    pub fn web3_api(&self) -> Web3Api<()> { Web3Api::new(()) }

    pub fn register_web3(&mut self) -> &mut Self {
        let web3api = self.web3_api();
        self.modules
            .insert(EthRpcModule::Web3, web3api.into_rpc().into());
        self
    }

    pub fn trace_api(&self) -> TraceApi { TraceApi::new() }

    pub fn debug_api(&self) -> DebugApi {
        DebugApi::new(self.consensus.clone(), self.max_estimation_gas_limit)
    }

    pub fn net_api(&self) -> NetApi<ChainInfo> {
        NetApi::new(ChainInfo::new(self.consensus.clone()))
    }

    /// Helper function to create a [`RpcModule`] if it's not `None`
    fn maybe_module(
        &mut self, config: Option<&RpcModuleSelection>,
    ) -> Option<RpcModule<()>> {
        config.map(|config| self.module_for(config))
    }

    /// Populates a new [`RpcModule`] based on the selected [`EthRpcModule`]s in
    /// the given [`RpcModuleSelection`]
    pub fn module_for(&mut self, config: &RpcModuleSelection) -> RpcModule<()> {
        let mut module = RpcModule::new(());
        let all_methods = self.eth_methods(config.iter_selection());
        for methods in all_methods {
            module.merge(methods).expect("No conflicts");
        }
        module
    }

    pub fn eth_methods(
        &mut self, namespaces: impl Iterator<Item = EthRpcModule>,
    ) -> Vec<Methods> {
        let namespaces: Vec<_> = namespaces.collect();
        namespaces
            .iter()
            .copied()
            .map(|namespace| {
                self.modules
                    .entry(namespace)
                    .or_insert_with(|| match namespace {
                        EthRpcModule::Debug => DebugApi::new(
                            self.consensus.clone(),
                            self.max_estimation_gas_limit,
                        )
                        .into_rpc()
                        .into(),
                        EthRpcModule::Eth => EthApi::new(
                            self.config.clone(),
                            self.consensus.clone(),
                            self.sync.clone(),
                            self.tx_pool.clone(),
                        )
                        .into_rpc()
                        .into(),
                        EthRpcModule::Net => {
                            NetApi::new(ChainInfo::new(self.consensus.clone()))
                                .into_rpc()
                                .into()
                        }
                        EthRpcModule::Trace => {
                            TraceApi::new().into_rpc().into()
                        }
                        EthRpcModule::Web3 => {
                            Web3Api::new(()).into_rpc().into()
                        }
                        EthRpcModule::Rpc => RPCApi::new(
                            namespaces
                                .iter()
                                .map(|module| {
                                    (module.to_string(), "1.0".to_string())
                                })
                                .collect(),
                        )
                        .into_rpc()
                        .into(),
                    })
                    .clone()
            })
            .collect::<Vec<_>>()
    }
}

/// A builder type for configuring and launching the servers that will handle
/// RPC requests.
///
/// Supported server transports are:
///    - http
///    - ws
///
/// Http and WS share the same settings: [`ServerBuilder`].
///
/// Once the [`RpcModule`] is built via [`RpcModuleBuilder`] the servers can be
/// started, See also [`ServerBuilder::build`] and
/// [`Server::start`](jsonrpsee::server::Server::start).
#[derive(Debug)]
pub struct RpcServerConfig<RpcMiddleware = Identity> {
    /// Configs for JSON-RPC Http.
    http_server_config: Option<ServerBuilder<Identity, Identity>>,
    /// Allowed CORS Domains for http
    http_cors_domains: Option<String>,
    /// Address where to bind the http server to
    http_addr: Option<SocketAddr>,
    /// Configs for WS server
    ws_server_config: Option<ServerBuilder<Identity, Identity>>,
    /// Allowed CORS Domains for ws.
    ws_cors_domains: Option<String>,
    /// Address where to bind the ws server to
    ws_addr: Option<SocketAddr>,
    /// Configurable RPC middleware
    #[allow(dead_code)]
    rpc_middleware: RpcServiceBuilder<RpcMiddleware>,
}

impl Default for RpcServerConfig<Identity> {
    fn default() -> Self {
        Self {
            http_server_config: None,
            http_cors_domains: None,
            http_addr: None,
            ws_server_config: None,
            ws_cors_domains: None,
            ws_addr: None,
            rpc_middleware: RpcServiceBuilder::new(),
        }
    }
}

impl RpcServerConfig {
    /// Creates a new config with only http set
    pub fn http(config: ServerBuilder<Identity, Identity>) -> Self {
        Self::default().with_http(config)
    }

    /// Creates a new config with only ws set
    pub fn ws(config: ServerBuilder<Identity, Identity>) -> Self {
        Self::default().with_ws(config)
    }

    /// Configures the http server
    ///
    /// Note: this always configures an [`EthSubscriptionIdProvider`]
    /// [`IdProvider`] for convenience. To set a custom [`IdProvider`],
    /// please use [`Self::with_id_provider`].
    pub fn with_http(
        mut self, config: ServerBuilder<Identity, Identity>,
    ) -> Self {
        self.http_server_config =
            Some(config.set_id_provider(EthSubscriptionIdProvider::default()));
        self
    }

    /// Configures the ws server
    ///
    /// Note: this always configures an [`EthSubscriptionIdProvider`]
    /// [`IdProvider`] for convenience. To set a custom [`IdProvider`],
    /// please use [`Self::with_id_provider`].
    pub fn with_ws(
        mut self, config: ServerBuilder<Identity, Identity>,
    ) -> Self {
        self.ws_server_config =
            Some(config.set_id_provider(EthSubscriptionIdProvider::default()));
        self
    }
}

impl<RpcMiddleware> RpcServerConfig<RpcMiddleware> {
    /// Configure rpc middleware
    pub fn set_rpc_middleware<T>(
        self, rpc_middleware: RpcServiceBuilder<T>,
    ) -> RpcServerConfig<T> {
        RpcServerConfig {
            http_server_config: self.http_server_config,
            http_cors_domains: self.http_cors_domains,
            http_addr: self.http_addr,
            ws_server_config: self.ws_server_config,
            ws_cors_domains: self.ws_cors_domains,
            ws_addr: self.ws_addr,
            rpc_middleware,
        }
    }

    /// Configure the cors domains for http _and_ ws
    pub fn with_cors(self, cors_domain: Option<String>) -> Self {
        self.with_http_cors(cors_domain.clone())
            .with_ws_cors(cors_domain)
    }

    /// Configure the cors domains for WS
    pub fn with_ws_cors(mut self, cors_domain: Option<String>) -> Self {
        self.ws_cors_domains = cors_domain;
        self
    }

    /// Configure the cors domains for HTTP
    pub fn with_http_cors(mut self, cors_domain: Option<String>) -> Self {
        self.http_cors_domains = cors_domain;
        self
    }

    /// Configures the [`SocketAddr`] of the http server
    ///
    /// Default is [`Ipv4Addr::LOCALHOST`] and
    pub const fn with_http_address(mut self, addr: SocketAddr) -> Self {
        self.http_addr = Some(addr);
        self
    }

    /// Configures the [`SocketAddr`] of the ws server
    ///
    /// Default is [`Ipv4Addr::LOCALHOST`] and
    pub const fn with_ws_address(mut self, addr: SocketAddr) -> Self {
        self.ws_addr = Some(addr);
        self
    }

    /// Sets a custom [`IdProvider`] for all configured transports.
    ///
    /// By default all transports use [`EthSubscriptionIdProvider`]
    pub fn with_id_provider<I>(mut self, id_provider: I) -> Self
    where I: IdProvider + Clone + 'static {
        if let Some(http) = self.http_server_config {
            self.http_server_config =
                Some(http.set_id_provider(id_provider.clone()));
        }
        if let Some(ws) = self.ws_server_config {
            self.ws_server_config =
                Some(ws.set_id_provider(id_provider.clone()));
        }

        self
    }

    /// Returns true if any server is configured.
    ///
    /// If no server is configured, no server will be launched on
    /// [`RpcServerConfig::start`].
    pub const fn has_server(&self) -> bool {
        self.http_server_config.is_some() || self.ws_server_config.is_some()
    }

    /// Returns the [`SocketAddr`] of the http server
    pub const fn http_address(&self) -> Option<SocketAddr> { self.http_addr }

    /// Returns the [`SocketAddr`] of the ws server
    pub const fn ws_address(&self) -> Option<SocketAddr> { self.ws_addr }

    // Builds and starts the configured server(s): http, ws, ipc.
    //
    // If both http and ws are on the same port, they are combined into one
    // server.
    //
    // Returns the [`RpcServerHandle`] with the handle to the started servers.
    pub async fn start(
        self, modules: &TransportRpcModules,
    ) -> Result<RpcServerHandle, RpcError> {
        let mut http_handle = None;
        let mut ws_handle = None;

        let http_socket_addr =
            self.http_addr.unwrap_or(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                constants::DEFAULT_HTTP_PORT,
            )));

        let ws_socket_addr = self.ws_addr.unwrap_or(SocketAddr::V4(
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, constants::DEFAULT_WS_PORT),
        ));

        // If both are configured on the same port, we combine them into one
        // server.
        if self.http_addr == self.ws_addr
            && self.http_server_config.is_some()
            && self.ws_server_config.is_some()
        {
            // let cors = match (self.ws_cors_domains.as_ref(),
            // self.http_cors_domains.as_ref()) {
            //     (Some(ws_cors), Some(http_cors)) => {
            //         if ws_cors.trim() != http_cors.trim() {
            //             return
            // Err(WsHttpSamePortError::ConflictingCorsDomains {
            //                 http_cors_domains: Some(http_cors.clone()),
            //                 ws_cors_domains: Some(ws_cors.clone()),
            //             }
            //             .into());
            //         }
            //         Some(ws_cors)
            //     }
            //     (a, b) => a.or(b),
            // }
            // .cloned();

            // we merge this into one server using the http setup
            modules.config.ensure_ws_http_identical()?;

            if let Some(builder) = self.http_server_config {
                let server = builder
                    // .set_http_middleware(
                    //     tower::ServiceBuilder::new()
                    //         .option_layer(Self::maybe_cors_layer(cors)?)
                    //         .option_layer(Self::maybe_jwt_layer(self.
                    // jwt_secret)), )
                    // .set_rpc_middleware(
                    //     self.rpc_middleware.clone().layer(
                    //         modules
                    //             .http
                    //             .as_ref()
                    //             .or(modules.ws.as_ref())
                    //             .map(RpcRequestMetrics::same_port)
                    //             .unwrap_or_default(),
                    //     ),
                    // )
                    .build(http_socket_addr)
                    .await
                    .map_err(|err| {
                        RpcError::server_error(
                            err,
                            ServerKind::WsHttp(http_socket_addr),
                        )
                    })?;
                let addr = server.local_addr().map_err(|err| {
                    RpcError::server_error(
                        err,
                        ServerKind::WsHttp(http_socket_addr),
                    )
                })?;
                if let Some(module) =
                    modules.http.as_ref().or(modules.ws.as_ref())
                {
                    let handle = server.start(module.clone());
                    http_handle = Some(handle.clone());
                    ws_handle = Some(handle);
                }
                return Ok(RpcServerHandle {
                    http_local_addr: Some(addr),
                    ws_local_addr: Some(addr),
                    http: http_handle,
                    ws: ws_handle,
                });
            }
        }

        let mut ws_local_addr = None;
        let mut ws_server = None;
        let mut http_local_addr = None;
        let mut http_server = None;

        if let Some(builder) = self.ws_server_config {
            let server = builder
                .ws_only()
                // .set_http_middleware(
                //     tower::ServiceBuilder::new()
                //         .option_layer(Self::maybe_cors_layer(self.
                // ws_cors_domains.clone())?)
                //         .option_layer(Self::maybe_jwt_layer(self.
                // jwt_secret)), )
                // .set_rpc_middleware(
                //     self.rpc_middleware
                //         .clone()
                //         .layer(modules.ws.as_ref().
                // map(RpcRequestMetrics::ws).unwrap_or_default()),
                // )
                .build(ws_socket_addr)
                .await
                .map_err(|err| {
                    RpcError::server_error(err, ServerKind::WS(ws_socket_addr))
                })?;

            let addr = server.local_addr().map_err(|err| {
                RpcError::server_error(err, ServerKind::WS(ws_socket_addr))
            })?;

            ws_local_addr = Some(addr);
            ws_server = Some(server);
        }

        if let Some(builder) = self.http_server_config {
            let server = builder
                .http_only()
                // .set_http_middleware(
                //     tower::ServiceBuilder::new()
                //         .option_layer(Self::maybe_cors_layer(self.
                // http_cors_domains.clone())?)
                //         .option_layer(Self::maybe_jwt_layer(self.
                // jwt_secret)), )
                // .set_rpc_middleware(
                //     self.rpc_middleware.clone().layer(
                //         modules.http.as_ref().map(RpcRequestMetrics::http).
                // unwrap_or_default(),     ),
                // )
                .build(http_socket_addr)
                .await
                .map_err(|err| {
                    RpcError::server_error(
                        err,
                        ServerKind::Http(http_socket_addr),
                    )
                })?;
            let local_addr = server.local_addr().map_err(|err| {
                RpcError::server_error(err, ServerKind::Http(http_socket_addr))
            })?;
            http_local_addr = Some(local_addr);
            http_server = Some(server);
        }

        http_handle = http_server.map(|http_server| {
            http_server.start(modules.http.clone().expect("http server error"))
        });
        ws_handle = ws_server.map(|ws_server| {
            ws_server.start(modules.ws.clone().expect("ws server error"))
        });
        Ok(RpcServerHandle {
            http_local_addr,
            ws_local_addr,
            http: http_handle,
            ws: ws_handle,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EthConfig {}

/// Bundles settings for modules
#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RpcModuleConfig {
    /// `eth` namespace settings
    eth: EthConfig,
}

// === impl RpcModuleConfig ===

impl RpcModuleConfig {
    /// Convenience method to create a new [`RpcModuleConfigBuilder`]
    pub fn builder() -> RpcModuleConfigBuilder {
        RpcModuleConfigBuilder::default()
    }

    /// Returns a new RPC module config given the eth namespace config
    pub const fn new(eth: EthConfig) -> Self { Self { eth } }

    /// Get a reference to the eth namespace config
    pub const fn eth(&self) -> &EthConfig { &self.eth }

    /// Get a mutable reference to the eth namespace config
    pub fn eth_mut(&mut self) -> &mut EthConfig { &mut self.eth }
}

/// Configures [`RpcModuleConfig`]
#[derive(Clone, Debug, Default)]
pub struct RpcModuleConfigBuilder {
    eth: Option<EthConfig>,
}

// === impl RpcModuleConfigBuilder ===

impl RpcModuleConfigBuilder {
    /// Configures a custom eth namespace config
    pub const fn eth(mut self, eth: EthConfig) -> Self {
        self.eth = Some(eth);
        self
    }

    /// Consumes the type and creates the [`RpcModuleConfig`]
    pub fn build(self) -> RpcModuleConfig {
        let Self { eth } = self;
        RpcModuleConfig {
            eth: eth.unwrap_or_default(),
        }
    }

    /// Get a reference to the eth namespace config, if any
    pub const fn get_eth(&self) -> &Option<EthConfig> { &self.eth }

    /// Get a mutable reference to the eth namespace config, if any
    pub fn eth_mut(&mut self) -> &mut Option<EthConfig> { &mut self.eth }

    /// Get the eth namespace config, creating a default if none is set
    pub fn eth_mut_or_default(&mut self) -> &mut EthConfig {
        self.eth.get_or_insert_with(EthConfig::default)
    }
}

/// Holds modules to be installed per transport type
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TransportRpcModuleConfig {
    /// http module configuration
    http: Option<RpcModuleSelection>,
    /// ws module configuration
    ws: Option<RpcModuleSelection>,
    /// Config for the modules
    config: Option<RpcModuleConfig>,
}

impl TransportRpcModuleConfig {
    /// Creates a new config with only http set
    pub fn set_http(http: impl Into<RpcModuleSelection>) -> Self {
        Self::default().with_http(http)
    }

    /// Creates a new config with only ws set
    pub fn set_ws(ws: impl Into<RpcModuleSelection>) -> Self {
        Self::default().with_ws(ws)
    }

    /// Sets the [`RpcModuleSelection`] for the http transport.
    pub fn with_http(mut self, http: impl Into<RpcModuleSelection>) -> Self {
        self.http = Some(http.into());
        self
    }

    /// Sets the [`RpcModuleSelection`] for the ws transport.
    pub fn with_ws(mut self, ws: impl Into<RpcModuleSelection>) -> Self {
        self.ws = Some(ws.into());
        self
    }

    /// Sets a custom [`RpcModuleConfig`] for the configured modules.
    pub const fn with_config(mut self, config: RpcModuleConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Get a mutable reference to the
    pub fn http_mut(&mut self) -> &mut Option<RpcModuleSelection> {
        &mut self.http
    }

    /// Get a mutable reference to the
    pub fn ws_mut(&mut self) -> &mut Option<RpcModuleSelection> { &mut self.ws }

    /// Get a mutable reference to the
    pub fn config_mut(&mut self) -> &mut Option<RpcModuleConfig> {
        &mut self.config
    }

    /// Returns true if no transports are configured
    pub const fn is_empty(&self) -> bool {
        self.http.is_none() && self.ws.is_none()
    }

    /// Returns the [`RpcModuleSelection`] for the http transport
    pub const fn http(&self) -> Option<&RpcModuleSelection> {
        self.http.as_ref()
    }

    /// Returns the [`RpcModuleSelection`] for the ws transport
    pub const fn ws(&self) -> Option<&RpcModuleSelection> { self.ws.as_ref() }

    /// Returns the [`RpcModuleConfig`] for the configured modules
    pub const fn config(&self) -> Option<&RpcModuleConfig> {
        self.config.as_ref()
    }

    /// Ensures that both http and ws are configured and that they are
    /// configured to use the same port.
    fn ensure_ws_http_identical(&self) -> Result<(), WsHttpSamePortError> {
        if RpcModuleSelection::are_identical(
            self.http.as_ref(),
            self.ws.as_ref(),
        ) {
            Ok(())
        } else {
            let http_modules = self
                .http
                .as_ref()
                .map(RpcModuleSelection::to_selection)
                .unwrap_or_default();
            let ws_modules = self
                .ws
                .as_ref()
                .map(RpcModuleSelection::to_selection)
                .unwrap_or_default();

            let http_not_ws =
                http_modules.difference(&ws_modules).copied().collect();
            let ws_not_http =
                ws_modules.difference(&http_modules).copied().collect();
            let overlap =
                http_modules.intersection(&ws_modules).copied().collect();

            Err(WsHttpSamePortError::ConflictingModules(Box::new(
                ConflictingModules {
                    overlap,
                    http_not_ws,
                    ws_not_http,
                },
            )))
        }
    }
}

/// Holds installed modules per transport type.
#[derive(Debug, Clone, Default)]
pub struct TransportRpcModules<Context = ()> {
    /// The original config
    config: TransportRpcModuleConfig,
    /// rpcs module for http
    http: Option<RpcModule<Context>>,
    /// rpcs module for ws
    ws: Option<RpcModule<Context>>,
}

// === impl TransportRpcModules ===

impl TransportRpcModules {
    /// Returns the [`TransportRpcModuleConfig`] used to configure this
    /// instance.
    pub const fn module_config(&self) -> &TransportRpcModuleConfig {
        &self.config
    }

    /// Merge the given [Methods] in the configured http methods.
    ///
    /// Fails if any of the methods in other is present already.
    ///
    /// Returns [Ok(false)] if no http transport is configured.
    pub fn merge_http(
        &mut self, other: impl Into<Methods>,
    ) -> Result<bool, RegisterMethodError> {
        if let Some(ref mut http) = self.http {
            return http.merge(other.into()).map(|_| true);
        }
        Ok(false)
    }

    /// Merge the given [Methods] in the configured ws methods.
    ///
    /// Fails if any of the methods in other is present already.
    ///
    /// Returns [Ok(false)] if no ws transport is configured.
    pub fn merge_ws(
        &mut self, other: impl Into<Methods>,
    ) -> Result<bool, RegisterMethodError> {
        if let Some(ref mut ws) = self.ws {
            return ws.merge(other.into()).map(|_| true);
        }
        Ok(false)
    }

    /// Merge the given [Methods] in all configured methods.
    ///
    /// Fails if any of the methods in other is present already.
    pub fn merge_configured(
        &mut self, other: impl Into<Methods>,
    ) -> Result<(), RegisterMethodError> {
        let other = other.into();
        self.merge_http(other.clone())?;
        self.merge_ws(other.clone())?;
        Ok(())
    }

    /// Removes the method with the given name from the configured http methods.
    ///
    /// Returns `true` if the method was found and removed, `false` otherwise.
    ///
    /// Be aware that a subscription consist of two methods, `subscribe` and
    /// `unsubscribe` and it's the caller responsibility to remove both
    /// `subscribe` and `unsubscribe` methods for subscriptions.
    pub fn remove_http_method(&mut self, method_name: &'static str) -> bool {
        if let Some(http_module) = &mut self.http {
            http_module.remove_method(method_name).is_some()
        } else {
            false
        }
    }

    /// Removes the method with the given name from the configured ws methods.
    ///
    /// Returns `true` if the method was found and removed, `false` otherwise.
    ///
    /// Be aware that a subscription consist of two methods, `subscribe` and
    /// `unsubscribe` and it's the caller responsibility to remove both
    /// `subscribe` and `unsubscribe` methods for subscriptions.
    pub fn remove_ws_method(&mut self, method_name: &'static str) -> bool {
        if let Some(ws_module) = &mut self.ws {
            ws_module.remove_method(method_name).is_some()
        } else {
            false
        }
    }

    /// Removes the method with the given name from all configured transports.
    ///
    /// Returns `true` if the method was found and removed, `false` otherwise.
    pub fn remove_method_from_configured(
        &mut self, method_name: &'static str,
    ) -> bool {
        let http_removed = self.remove_http_method(method_name);
        let ws_removed = self.remove_ws_method(method_name);

        http_removed || ws_removed
    }
}

/// A handle to the spawned servers.
///
/// When this type is dropped or [`RpcServerHandle::stop`] has been called the
/// server will be stopped.
#[derive(Clone, Debug)]
#[must_use = "Server stops if dropped"]
pub struct RpcServerHandle {
    /// The address of the http/ws server
    http_local_addr: Option<SocketAddr>,
    ws_local_addr: Option<SocketAddr>,
    http: Option<ServerHandle>,
    ws: Option<ServerHandle>,
}

impl RpcServerHandle {
    /// Returns the [`SocketAddr`] of the http server if started.
    pub const fn http_local_addr(&self) -> Option<SocketAddr> {
        self.http_local_addr
    }

    /// Returns the [`SocketAddr`] of the ws server if started.
    pub const fn ws_local_addr(&self) -> Option<SocketAddr> {
        self.ws_local_addr
    }

    /// Tell the server to stop without waiting for the server to stop.
    pub fn stop(self) -> Result<(), AlreadyStoppedError> {
        if let Some(handle) = self.http {
            handle.stop()?
        }

        if let Some(handle) = self.ws {
            handle.stop()?
        }

        Ok(())
    }

    /// Returns the url to the http server
    pub fn http_url(&self) -> Option<String> {
        self.http_local_addr.map(|addr| format!("http://{addr}"))
    }

    /// Returns the url to the ws server
    pub fn ws_url(&self) -> Option<String> {
        self.ws_local_addr.map(|addr| format!("ws://{addr}"))
    }
}
