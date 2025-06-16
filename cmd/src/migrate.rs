use std::sync::Arc;

use abi::config::Config;
use db::message::MsgRecBoxRepo;
use futures::TryStreamExt;
use mongodb::{
    bson::{doc, Document},
    options::FindOptions,
    Client,
};
use tokio::sync::{mpsc, Semaphore};
use tracing::{debug, error, info};

use abi::message::Msg;

/// 消息迁移器
/// 将消息从单一集合迁移到分片集合
pub struct MessageMigrator {
    config: Config,
    source_client: Client,
    target_repo: Arc<dyn MsgRecBoxRepo>,
    batch_size: usize,
    concurrency: usize,
}

impl MessageMigrator {
    /// 创建新的消息迁移器
    pub async fn new(config: Config, target_repo: Arc<dyn MsgRecBoxRepo>) -> Self {
        let source_client = Client::with_uri_str(&config.db.mongodb.url())
            .await
            .expect("Failed to connect to MongoDB for migration");

        Self {
            config,
            source_client,
            target_repo,
            batch_size: 100,
            concurrency: 10,
        }
    }

    /// 设置批量大小
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// 设置并发度
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency;
        self
    }

    /// 开始迁移过程
    pub async fn migrate(&self) -> Result<(), anyhow::Error> {
        info!("Starting message migration from single collection to sharded collections");

        let db = self
            .source_client
            .database(&self.config.db.mongodb.database);
        let source_coll = db.collection::<Document>("single_msg_box");

        // 获取总消息数
        let total_count = source_coll.count_documents(doc! {}, None).await?;
        info!("Found {} messages to migrate", total_count);

        // 创建进度跟踪通道
        let (tx, mut rx) = mpsc::channel::<usize>(100);

        // 启动进度报告器
        let progress_reporter = tokio::spawn(async move {
            let mut processed = 0;
            while let Some(batch_count) = rx.recv().await {
                processed += batch_count;
                info!(
                    "Migration progress: {}/{} messages ({:.2}%)",
                    processed,
                    total_count,
                    (processed as f64 / total_count as f64) * 100.0
                );
            }
        });

        // 设置并发限制
        let semaphore = Arc::new(Semaphore::new(self.concurrency));

        // 查询所有消息并批量处理
        let mut cursor = source_coll
            .find(
                doc! {},
                FindOptions::builder()
                    .batch_size(self.batch_size as u32)
                    .build(),
            )
            .await?;

        let mut batch = Vec::with_capacity(self.batch_size);
        let mut tasks = Vec::new();

        while let Some(doc) = cursor.try_next().await? {
            batch.push(doc);

            if batch.len() >= self.batch_size {
                let current_batch =
                    std::mem::replace(&mut batch, Vec::with_capacity(self.batch_size));
                let target_repo = self.target_repo.clone();
                let permit = semaphore.clone().acquire_owned().await?;
                let batch_tx = tx.clone();

                let task = tokio::spawn(async move {
                    let batch_size = current_batch.len();
                    let result = process_batch(current_batch, target_repo).await;

                    if let Err(e) = &result {
                        error!("Failed to process batch: {:?}", e);
                    }

                    // 报告进度
                    if batch_tx.send(batch_size).await.is_err() {
                        error!("Failed to send progress update");
                    }

                    // 释放信号量
                    drop(permit);
                    result
                });

                tasks.push(task);
            }
        }

        // 处理剩余的消息
        if !batch.is_empty() {
            let target_repo = self.target_repo.clone();
            let permit = semaphore.acquire_owned().await?;
            let batch_size = batch.len();
            let batch_tx = tx.clone();

            let task = tokio::spawn(async move {
                let result = process_batch(batch, target_repo).await;

                if let Err(e) = &result {
                    error!("Failed to process final batch: {:?}", e);
                }

                // 报告进度
                if batch_tx.send(batch_size).await.is_err() {
                    error!("Failed to send progress update");
                }

                // 释放信号量
                drop(permit);
                result
            });

            tasks.push(task);
        }

        // 等待所有任务完成
        for task in tasks {
            if let Err(e) = task.await? {
                error!("Task error: {:?}", e);
            }
        }

        // 等待进度报告器完成
        drop(tx);
        if let Err(e) = progress_reporter.await {
            error!("Progress reporter error: {:?}", e);
        }

        info!("Migration completed successfully");
        Ok(())
    }
}

/// 处理一批消息
async fn process_batch(
    batch: Vec<Document>,
    target_repo: Arc<dyn MsgRecBoxRepo>,
) -> Result<(), anyhow::Error> {
    for doc in batch {
        match Msg::try_from(doc) {
            Ok(msg) => {
                if let Err(e) = target_repo.save_message(&msg).await {
                    error!("Failed to save message {}: {:?}", msg.server_id, e);
                    // 继续处理，不中止整个批次
                }
            }
            Err(e) => {
                debug!("Failed to parse message document: {:?}", e);
                // 继续处理，不中止整个批次
            }
        }
    }

    Ok(())
}

/// 启动迁移过程
pub async fn run_migration(config: Config) -> Result<(), anyhow::Error> {
    // 创建目标仓库
    // 注意：我们强制使用分片实现，不管配置如何
    let mut sharding_config = config.clone();

    // 确保我们使用的是分片实现
    if sharding_config.db.mongodb.use_sharding.is_none() {
        sharding_config.db.mongodb.use_sharding = Some(true);
    }

    if sharding_config.db.mongodb.user_shards.is_none() {
        sharding_config.db.mongodb.user_shards = Some(10);
    }

    // 获取目标仓库
    let target_repo = db::msg_rec_box_repo(&sharding_config).await;

    // 创建迁移器并执行迁移
    let migrator = MessageMigrator::new(config, target_repo)
        .await
        .with_batch_size(500)
        .with_concurrency(5);

    migrator.migrate().await?;

    Ok(())
}
