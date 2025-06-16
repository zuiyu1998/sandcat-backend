use friend::FriendRepo;
use group::GroupStoreRepo;
use seq::SeqRepo;
use tracing::info;

use abi::{config::Config, message::MsgType};
use user::UserRepo;

mod mongodb;
mod postgres;

pub mod friend;
pub mod group;
pub mod message;
// pub mod rpc;
pub mod seq;
pub mod user;

use std::sync::Arc;

use message::{MsgRecBoxCleaner, MsgRecBoxRepo, MsgStoreRepo};
use sqlx::PgPool;

/// shall we create a structure to hold everything we need?
/// like db pool and mongodb's database
#[derive(Debug)]
pub struct DbRepo {
    pub msg: Box<dyn MsgStoreRepo>,
    pub group: Box<dyn GroupStoreRepo>,
    pub user: Box<dyn UserRepo>,
    pub friend: Box<dyn FriendRepo>,
    pub seq: Box<dyn SeqRepo>,
}

impl DbRepo {
    pub async fn new(config: &Config) -> Self {
        let pool = PgPool::connect(&config.db.postgres.url()).await.unwrap();
        let seq_step = config.redis.seq_step;

        let msg = Box::new(postgres::PostgresMessage::new(pool.clone()));
        let user = Box::new(postgres::PostgresUser::new(pool.clone(), seq_step));
        let friend = Box::new(postgres::PostgresFriend::new(pool.clone()));
        let group = Box::new(postgres::PostgresGroup::new(pool.clone()));
        let seq = Box::new(postgres::PostgresSeq::new(pool, seq_step));
        Self {
            msg,
            group,
            user,
            friend,
            seq,
        }
    }
}

/// 创建消息接收盒子仓库，根据配置选择适当的实现
pub async fn msg_rec_box_repo(config: &Config) -> Arc<dyn MsgRecBoxRepo> {
    // 检查是否启用了分片
    if let Some(true) = config.db.mongodb.use_sharding {
        // 获取分片数，默认为10
        let user_shards = config.db.mongodb.user_shards.unwrap_or(10) as u8;
        info!("Using sharded message box with {} user shards", user_shards);
        Arc::new(mongodb::HybridShardedMsgBox::from_config(config, user_shards).await)
    } else {
        info!("Using legacy message box (non-sharded)");
        Arc::new(mongodb::MsgBox::from_config(config).await)
    }
}

/// 创建消息清理器，根据配置选择适当的实现
pub async fn msg_rec_box_cleaner(config: &Config) -> Arc<dyn MsgRecBoxCleaner> {
    // 保持与消息盒子相同的实现选择
    if let Some(true) = config.db.mongodb.use_sharding {
        let user_shards = config.db.mongodb.user_shards.unwrap_or(10) as u8;
        Arc::new(mongodb::HybridShardedMsgBox::from_config(config, user_shards).await)
    } else {
        Arc::new(mongodb::MsgBox::from_config(config).await)
    }
}

pub async fn clean_receive_box(config: &Config) {
    let types: Vec<i32> = config
        .db
        .mongodb
        .clean
        .except_types
        .iter()
        .filter_map(|v| MsgType::from_str_name(v))
        .map(|v| v as i32)
        .collect();
    let period = config.db.mongodb.clean.period;

    let msg_box = msg_rec_box_cleaner(config).await;
    info!(
        "clean receive box task started, and the period is {period}s; the except types is {:?}",
        types
    );
    msg_box.clean_receive_box(period, types);
}
