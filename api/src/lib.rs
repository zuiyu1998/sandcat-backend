use std::net::SocketAddr;
use std::sync::Arc;

use xdb::searcher_init;

use crate::api_utils::lb;
use abi::config::{Config, MailConfig, WsServerConfig};
use abi::errors::Error;
use abi::message::chat_service_client::ChatServiceClient;
use abi::message::db_service_client::DbServiceClient;
use abi::message::msg_service_client::MsgServiceClient;
use cache::Cache;
use oss::Oss;
use utils::service_discovery::LbWithServiceDiscovery;

mod api_utils;
pub(crate) mod handlers;
pub(crate) mod routes;

#[derive(Clone, Debug)]
pub struct AppState {
    pub db_rpc: DbServiceClient<LbWithServiceDiscovery>,
    pub ws_rpc: MsgServiceClient<LbWithServiceDiscovery>,
    pub chat_rpc: ChatServiceClient<LbWithServiceDiscovery>,
    pub cache: Arc<dyn Cache>,
    pub oss: Arc<dyn Oss>,
    pub ws_lb: Arc<lb::LoadBalancer>,
    pub ws_config: WsServerConfig,
    pub mail_config: MailConfig,
    pub jwt_secret: String,
}

impl AppState {
    pub async fn new(config: &Config) -> Self {
        let ws_rpc = Self::get_ws_rpc_client(config).await.unwrap();

        let db_rpc = Self::get_db_rpc_client(config).await.unwrap();
        let chat_rpc = Self::get_chat_rpc_client(config).await.unwrap();

        let cache = cache::cache(config);

        let oss = oss::oss(config).await;

        let ws_config = config.websocket.clone();

        let mail_config = config.mail.clone();

        let register = utils::service_register_center(config);

        let ws_lb = Arc::new(
            lb::LoadBalancer::new(
                config.websocket.name.clone(),
                config.server.ws_lb_strategy.clone(),
                register,
            )
            .await,
        );

        Self {
            ws_rpc,
            db_rpc,
            cache,
            oss,
            ws_lb,
            ws_config,
            mail_config,
            jwt_secret: config.server.jwt_secret.clone(),
            chat_rpc,
        }
    }

    async fn get_chat_rpc_client(
        config: &Config,
    ) -> Result<ChatServiceClient<LbWithServiceDiscovery>, Error> {
        // use service register center to get ws rpc url
        let channel = utils::get_channel_with_config(
            config,
            &config.rpc.chat.name,
            &config.rpc.chat.protocol,
        )
        .await?;
        let chat_rpc = ChatServiceClient::new(channel);
        Ok(chat_rpc)
    }

    async fn get_db_rpc_client(
        config: &Config,
    ) -> Result<DbServiceClient<LbWithServiceDiscovery>, Error> {
        // use service register center to get ws rpc url
        let channel =
            utils::get_channel_with_config(config, &config.rpc.db.name, &config.rpc.db.protocol)
                .await?;
        let db_rpc = DbServiceClient::new(channel);
        Ok(db_rpc)
    }

    async fn get_ws_rpc_client(
        config: &Config,
    ) -> Result<MsgServiceClient<LbWithServiceDiscovery>, Error> {
        let channel =
            utils::get_channel_with_config(config, &config.rpc.ws.name, &config.rpc.ws.protocol)
                .await?;
        let ws_rpc = MsgServiceClient::new(channel);
        Ok(ws_rpc)
    }
}

pub async fn start(config: Config) {
    // init search ip region xdb
    searcher_init(Some(config.db.xdb.clone()));
    let state = AppState::new(&config).await;
    let app = routes::app_routes(state.clone());
    let listener = tokio::net::TcpListener::bind(&config.server.server_url())
        .await
        .unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
