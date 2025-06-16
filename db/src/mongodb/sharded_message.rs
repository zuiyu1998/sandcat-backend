use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Months, Utc};
use mongodb::bson::{Document, doc};
use mongodb::options::{FindOptions, IndexOptions};
use mongodb::{Client, Collection, IndexModel};
use tokio::sync::{RwLock, mpsc};
use tonic::codegen::tokio_stream::StreamExt;
use tracing::{debug, error, info, warn};

use abi::config::Config;
use abi::errors::Error;
use abi::message::{GroupMemSeq, Msg};

use crate::message::{MsgRecBoxCleaner, MsgRecBoxRepo};
use crate::mongodb::utils::to_doc;

/// 混合分片策略 - 基于时间和用户ID的分片
#[derive(Debug)]
pub struct HybridShardedMsgBox {
    /// MongoDB 客户端
    client: Client,
    /// 数据库名称
    db_name: String,
    /// 跟踪活跃的分片集合
    active_collections: Arc<RwLock<HashSet<String>>>,
    /// 缓存集合对象，避免频繁创建
    collection_cache: Arc<RwLock<HashMap<String, Collection<Document>>>>,
    /// 用户ID分片策略的分片数量
    user_shards: u8,
}

impl HybridShardedMsgBox {
    /// 创建新的混合分片消息存储
    pub async fn new(client: Client, db_name: String, user_shards: u8) -> Self {
        let instance = Self {
            client,
            db_name,
            active_collections: Arc::new(RwLock::new(HashSet::new())),
            collection_cache: Arc::new(RwLock::new(HashMap::new())),
            user_shards,
        };

        // 初始化系统，确保索引和集合已准备好
        if let Err(e) = instance.initialize_system().await {
            error!("Failed to initialize sharded message system: {:?}", e);
        }

        instance
    }

    /// 从配置创建实例
    pub async fn from_config(config: &Config, user_shards: u8) -> Self {
        let client = Client::with_uri_str(config.db.mongodb.url()).await.unwrap();

        Self::new(client, config.db.mongodb.database.clone(), user_shards).await
    }

    /// 初始化分片系统
    async fn initialize_system(&self) -> Result<(), Error> {
        // 获取当前时间前后3个月的集合，确保它们有正确的索引
        let now = Utc::now();

        for month_offset in -1..=3 {
            for user_range in 0..self.user_shards {
                let adjusted_date = now
                    .checked_add_months(Months::new(month_offset as u32))
                    .unwrap_or(now);

                let coll_name = self.get_collection_name(adjusted_date.timestamp(), user_range);

                // 创建或更新集合索引
                self.ensure_collection_indexed(&coll_name).await?;

                // 记录活跃分区
                let mut active_collections = self.active_collections.write().await;
                active_collections.insert(coll_name.clone());
            }
        }

        // 设置定期检查新分区的任务
        self.schedule_collection_maintenance();

        info!(
            "Initialized hybrid sharded message storage with {} user shards",
            self.user_shards
        );
        Ok(())
    }

    /// 确保指定的集合存在并有正确的索引
    async fn ensure_collection_indexed(&self, collection_name: &str) -> Result<(), Error> {
        let db = self.client.database(&self.db_name);
        let coll = db.collection::<Document>(collection_name);

        // 创建索引
        let index_models = vec![
            // 接收者ID和序列号的复合索引，用于高效查询用户消息
            IndexModel::builder()
                .keys(doc! {"receiver_id": 1, "seq": 1})
                .options(IndexOptions::builder().unique(false).build())
                .build(),
            // 发送时间索引，用于时间范围查询
            IndexModel::builder()
                .keys(doc! {"send_time": 1})
                .options(IndexOptions::builder().unique(false).build())
                .build(),
            // 发送者ID和发送序号的复合索引
            IndexModel::builder()
                .keys(doc! {"sender_id": 1, "send_seq": 1})
                .options(IndexOptions::builder().unique(false).build())
                .build(),
            // 服务器ID和接收者ID的复合索引，允许同一消息ID发送给多个接收者
            IndexModel::builder()
                .keys(doc! {"server_id": 1, "receiver_id": 1})
                .options(IndexOptions::builder().unique(true).sparse(true).build())
                .build(),
        ];

        for model in index_models {
            match coll.create_index(model, None).await {
                Ok(_) => {
                    debug!("Created index for collection {}", collection_name);
                }
                Err(e) => {
                    // 如果索引已存在，不要将其视为错误
                    if !e.to_string().contains("already exists") {
                        warn!("Failed to create index for {}: {:?}", collection_name, e);
                        return Err(e.into());
                    }
                }
            }
        }

        // 添加到集合缓存
        let mut cache = self.collection_cache.write().await;
        cache.insert(collection_name.to_string(), coll);

        Ok(())
    }

    /// 获取指定集合
    async fn get_collection(&self, collection_name: &str) -> Result<Collection<Document>, Error> {
        // 先检查缓存
        {
            let cache = self.collection_cache.read().await;
            if let Some(coll) = cache.get(collection_name) {
                return Ok(coll.clone());
            }
        }

        // 如果缓存未命中，创建并确保索引
        self.ensure_collection_indexed(collection_name).await?;

        // 再次从缓存获取
        let cache = self.collection_cache.read().await;
        if let Some(coll) = cache.get(collection_name) {
            Ok(coll.clone())
        } else {
            // 这种情况理论上不应发生
            Err(Error::internal_with_details("Failed to cache collection"))
        }
    }

    /// 根据时间和用户ID获取集合名
    fn get_collection_name(&self, timestamp: i64, user_range: u8) -> String {
        let dt = DateTime::<Utc>::from_timestamp(timestamp, 0).unwrap_or(Utc::now());
        let year_month = dt.format("%Y_%m");
        format!("msg_box_{}_{}", year_month, user_range)
    }

    /// 根据用户ID获取分片范围
    fn get_user_range(&self, user_id: &str) -> u8 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        user_id.hash(&mut hasher);
        (hasher.finish() % self.user_shards as u64) as u8
    }

    /// 设置定期检查和创建新集合的任务
    fn schedule_collection_maintenance(&self) {
        let client_clone = self.client.clone();
        let db_name = self.db_name.clone();
        let active_collections = self.active_collections.clone();
        let collection_cache = self.collection_cache.clone();
        let user_shards = self.user_shards;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(86400)); // 每天运行
            loop {
                interval.tick().await;

                // 检查未来3个月的分区
                let now = Utc::now();
                for month_offset in 0..=3 {
                    for user_range in 0..user_shards {
                        let adjusted_date = now
                            .checked_add_months(Months::new(month_offset as u32))
                            .unwrap_or(now);

                        let coll_name = {
                            let year_month = adjusted_date.format("%Y_%m");
                            format!("msg_box_{}_{}", year_month, user_range)
                        };

                        let mut active = active_collections.write().await;
                        if !active.contains(&coll_name) {
                            // 创建新分区索引
                            let db = client_clone.database(&db_name);
                            let coll = db.collection::<Document>(&coll_name);

                            let index_models = vec![
                                IndexModel::builder()
                                    .keys(doc! {"receiver_id": 1, "seq": 1})
                                    .options(IndexOptions::builder().unique(false).build())
                                    .build(),
                                IndexModel::builder()
                                    .keys(doc! {"send_time": 1})
                                    .options(IndexOptions::builder().unique(false).build())
                                    .build(),
                                IndexModel::builder()
                                    .keys(doc! {"sender_id": 1, "send_seq": 1})
                                    .options(IndexOptions::builder().unique(false).build())
                                    .build(),
                                IndexModel::builder()
                                    .keys(doc! {"server_id": 1, "receiver_id": 1})
                                    .options(
                                        IndexOptions::builder().unique(true).sparse(true).build(),
                                    )
                                    .build(),
                            ];

                            for model in index_models {
                                if let Err(e) = coll.create_index(model, None).await {
                                    // 忽略"索引已存在"错误
                                    if !e.to_string().contains("already exists") {
                                        error!("Failed to create index for {}: {:?}", coll_name, e);
                                    }
                                }
                            }

                            // 更新缓存
                            active.insert(coll_name.clone());
                            let mut cache = collection_cache.write().await;
                            cache.insert(coll_name.clone(), coll);

                            info!("Created new collection: {}", coll_name);
                        }
                    }
                }
            }
        });
    }
}

#[async_trait]
impl MsgRecBoxRepo for HybridShardedMsgBox {
    async fn save_message(&self, message: &Msg) -> Result<(), Error> {
        let user_range = self.get_user_range(&message.receiver_id);
        let coll_name = self.get_collection_name(message.send_time, user_range);

        let collection = self.get_collection(&coll_name).await?;
        collection.insert_one(to_doc(message)?, None).await?;

        Ok(())
    }

    async fn save_group_msg(&self, message: Msg, members: Vec<GroupMemSeq>) -> Result<(), Error> {
        let mut messages = Vec::with_capacity(members.len() + 1);

        // 为发送者保存一份消息
        let sender_range = self.get_user_range(&message.sender_id);
        let sender_coll_name = self.get_collection_name(message.send_time, sender_range);
        let sender_coll = self.get_collection(&sender_coll_name).await?;

        // 创建发送者的消息副本，将receiver_id设为sender_id，这样才能在发送者的消息盒子查询到
        let mut sender_msg = message.clone();
        sender_msg.receiver_id = sender_msg.sender_id.clone();

        messages.push((sender_coll, to_doc(&sender_msg)?));

        // 为每个接收成员保存消息
        let mut message_copy = message.clone(); // 使用clone以避免所有权问题
        message_copy.send_seq = 0; // 重置发送序列号

        for seq in members {
            message_copy.seq = seq.cur_seq;
            message_copy.receiver_id = seq.mem_id.clone();

            let receiver_range = self.get_user_range(&seq.mem_id);
            let receiver_coll_name =
                self.get_collection_name(message_copy.send_time, receiver_range);
            let receiver_coll = self.get_collection(&receiver_coll_name).await?;

            messages.push((receiver_coll.clone(), to_doc(&message_copy)?));
        }

        // 并行执行插入操作
        let mut handles = Vec::with_capacity(messages.len());

        for (collection, doc) in messages {
            let handle = tokio::spawn(async move { collection.insert_one(doc, None).await });
            handles.push(handle);
        }

        // 等待所有插入完成
        for handle in handles {
            handle.await.map_err(Error::internal)??;
        }

        Ok(())
    }

    async fn delete_message(&self, message_id: &str) -> Result<(), Error> {
        // 需要在所有活跃集合中查找并删除消息
        let active_collections = self.active_collections.read().await;

        for coll_name in active_collections.iter() {
            let collection = self.get_collection(coll_name).await?;
            let result = collection
                .delete_one(doc! {"server_id": message_id}, None)
                .await?;

            if result.deleted_count > 0 {
                return Ok(());
            }
        }

        // 如果未找到消息但操作请求成功，仍然返回成功
        Ok(())
    }

    async fn delete_messages(&self, user_id: &str, msg_seq: Vec<i64>) -> Result<(), Error> {
        if msg_seq.is_empty() {
            return Ok(());
        }

        // 计算用户的分片
        let user_range = self.get_user_range(user_id);

        // 获取当前和前1个月的集合
        let now = Utc::now();
        let collections = vec![
            self.get_collection_name(now.timestamp(), user_range),
            self.get_collection_name(
                now.checked_sub_months(Months::new(1))
                    .unwrap_or(now)
                    .timestamp(),
                user_range,
            ),
        ];

        let query = doc! {"receiver_id": user_id, "seq": {"$in": &msg_seq}};

        // 在可能的集合中执行删除
        for coll_name in collections {
            let collection = match self.get_collection(&coll_name).await {
                Ok(c) => c,
                Err(_) => continue, // 集合可能不存在，跳过
            };

            collection.delete_many(query.clone(), None).await?;
        }

        Ok(())
    }

    async fn get_message(&self, message_id: &str) -> Result<Option<Msg>, Error> {
        // 检查所有活跃集合
        let active_collections = self.active_collections.read().await;

        for coll_name in active_collections.iter() {
            let collection = self.get_collection(coll_name).await?;
            if let Some(doc) = collection
                .find_one(doc! {"server_id": message_id}, None)
                .await?
            {
                return Ok(Some(Msg::try_from(doc)?));
            }
        }

        Ok(None)
    }

    async fn get_messages_stream(
        &self,
        user_id: &str,
        start: i64,
        end: i64,
    ) -> Result<mpsc::Receiver<Result<Msg, Error>>, Error> {
        // 计算用户的分片
        let user_range = self.get_user_range(user_id);

        // 准备要查询的集合列表
        // 为简化实现，这里仅查询最近3个月的集合
        let now = Utc::now();
        let collections = vec![
            self.get_collection_name(now.timestamp(), user_range),
            self.get_collection_name(
                now.checked_sub_months(Months::new(1))
                    .unwrap_or(now)
                    .timestamp(),
                user_range,
            ),
            self.get_collection_name(
                now.checked_sub_months(Months::new(2))
                    .unwrap_or(now)
                    .timestamp(),
                user_range,
            ),
        ];

        // 创建管道
        let (tx, rx) = mpsc::channel(100);

        // 启动异步任务查询多个集合
        let query = doc! {
            "receiver_id": user_id,
            "seq": {
                "$gte": start,
                "$lte": end
            }
        };
        let option = FindOptions::builder().sort(Some(doc! {"seq": 1})).build();

        let client = self.client.clone();
        let db_name = self.db_name.clone();

        tokio::spawn(async move {
            // 从每个集合获取消息
            for coll_name in collections {
                let db = client.database(&db_name);

                // 检查集合是否存在
                if !db
                    .list_collection_names(None)
                    .await
                    .unwrap_or_default()
                    .contains(&coll_name)
                {
                    continue;
                }

                let coll = db.collection::<Document>(&coll_name);
                let mut cursor = match coll.find(query.clone(), option.clone()).await {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // 处理查询结果
                while let Some(result) = cursor.next().await {
                    match result {
                        Ok(doc) => match Msg::try_from(doc) {
                            Ok(msg) => {
                                if tx.send(Ok(msg)).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                if tx.send(Err(e)).await.is_err() {
                                    break;
                                }
                            }
                        },
                        Err(e) => {
                            if tx.send(Err(e.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn get_messages(&self, user_id: &str, start: i64, end: i64) -> Result<Vec<Msg>, Error> {
        // 计算用户的分片
        let user_range = self.get_user_range(user_id);

        // 准备要查询的集合
        let now = Utc::now();
        let collections = vec![
            self.get_collection_name(now.timestamp(), user_range),
            self.get_collection_name(
                now.checked_sub_months(Months::new(1))
                    .unwrap_or(now)
                    .timestamp(),
                user_range,
            ),
            self.get_collection_name(
                now.checked_sub_months(Months::new(2))
                    .unwrap_or(now)
                    .timestamp(),
                user_range,
            ),
        ];

        let query = doc! {
            "receiver_id": user_id,
            "seq": {
                "$gte": start,
                "$lte": end
            }
        };
        let option = FindOptions::builder().sort(Some(doc! {"seq": 1})).build();

        let mut all_messages = Vec::with_capacity((end - start) as usize);

        for coll_name in collections {
            let collection = match self.get_collection(&coll_name).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut cursor = collection.find(query.clone(), option.clone()).await?;

            while let Some(result) = cursor.next().await {
                let msg = Msg::try_from(result?)?;
                all_messages.push(msg);
            }
        }

        // 按序列号排序
        all_messages.sort_by_key(|msg| msg.seq);

        Ok(all_messages)
    }

    async fn get_msgs(
        &self,
        user_id: &str,
        send_start: i64,
        send_end: i64,
        rec_start: i64,
        rec_end: i64,
    ) -> Result<Vec<Msg>, Error> {
        // 计算用户的分片
        let user_range = self.get_user_range(user_id);

        // 准备集合
        let now = Utc::now();
        let collections = vec![
            self.get_collection_name(now.timestamp(), user_range),
            self.get_collection_name(
                now.checked_sub_months(Months::new(1))
                    .unwrap_or(now)
                    .timestamp(),
                user_range,
            ),
            self.get_collection_name(
                now.checked_sub_months(Months::new(2))
                    .unwrap_or(now)
                    .timestamp(),
                user_range,
            ),
        ];

        // 构建聚合管道
        let pipeline = vec![
            doc! {
                "$match": {
                    "$or": [
                        {
                            "receiver_id": user_id,
                            "seq": { "$gte": rec_start, "$lte": rec_end }
                        },
                        {
                            "sender_id": user_id,
                            "send_seq": { "$gte": send_start, "$lte": send_end }
                        }
                    ]
                }
            },
            doc! {
                "$addFields": {
                    "sort_field": {
                        "$cond": {
                            "if": { "$eq": ["$sender_id", user_id] },
                            "then": "$send_seq",
                            "else": "$seq"
                        }
                    }
                }
            },
            doc! {
                "$sort": {
                    "send_time": 1,
                    "sort_field": 1
                }
            },
        ];

        let len = send_end - send_start + (rec_end - rec_start);
        let mut messages = Vec::with_capacity(len as usize);

        // 在每个集合中执行聚合查询
        for coll_name in collections {
            let collection = match self.get_collection(&coll_name).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut cursor = collection.aggregate(pipeline.clone(), None).await?;

            while let Some(result) = cursor.next().await {
                let mut msg = Msg::try_from(result?)?;
                // 如果消息是用户发送的，将seq设为0
                if user_id == msg.sender_id {
                    msg.seq = 0;
                }
                messages.push(msg);
            }
        }

        // 最终排序
        messages.sort_by(|a, b| {
            a.send_time.cmp(&b.send_time).then_with(|| {
                let a_sort = if a.sender_id == user_id {
                    a.send_seq
                } else {
                    a.seq
                };
                let b_sort = if b.sender_id == user_id {
                    b.send_seq
                } else {
                    b.seq
                };
                a_sort.cmp(&b_sort)
            })
        });

        Ok(messages)
    }

    async fn msg_read(&self, user_id: &str, msg_seq: &[i64]) -> Result<(), Error> {
        if msg_seq.is_empty() {
            return Ok(());
        }

        // 计算用户的分片
        let user_range = self.get_user_range(user_id);

        // 准备集合
        let now = Utc::now();
        let collections = vec![
            self.get_collection_name(now.timestamp(), user_range),
            self.get_collection_name(
                now.checked_sub_months(Months::new(1))
                    .unwrap_or(now)
                    .timestamp(),
                user_range,
            ),
        ];

        let query = doc! {"receiver_id":{"$eq":user_id},"seq":{"$in":msg_seq}};
        let update = doc! {"$set":{"is_read":true}};

        // 在每个集合中执行更新
        for coll_name in collections {
            let collection = match self.get_collection(&coll_name).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            collection
                .update_many(query.clone(), update.clone(), None)
                .await?;
        }

        Ok(())
    }
}

impl MsgRecBoxCleaner for HybridShardedMsgBox {
    fn clean_receive_box(&self, period: i64, types: Vec<i32>) {
        // 克隆必要的字段以在新任务中使用
        let client = self.client.clone();
        let db_name = self.db_name.clone();
        let active_collections = self.active_collections.clone();

        tokio::spawn(async move {
            let retention_duration = chrono::Duration::days(period);
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(24 * 60 * 60)); // 每24小时执行一次

            loop {
                interval.tick().await;
                let now = Utc::now();
                let cutoff_time = (now - retention_duration).timestamp();

                // 获取所有活跃集合
                let collections = active_collections.read().await;
                let db = client.database(&db_name);

                for coll_name in collections.iter() {
                    // 验证集合是否存在
                    if !db
                        .list_collection_names(None)
                        .await
                        .unwrap_or_default()
                        .contains(coll_name)
                    {
                        continue;
                    }

                    let coll = db.collection::<Document>(coll_name);

                    let result = coll
                        .delete_many(
                            doc! {
                                "send_time": { "$lt": cutoff_time },
                                "msg_type": { "$nin": types.clone()}
                            },
                            None,
                        )
                        .await;

                    match result {
                        Ok(delete_result) => {
                            info!(
                                "Deleted {} expired messages from {}",
                                delete_result.deleted_count, coll_name
                            );
                        }
                        Err(e) => {
                            error!(
                                "Error deleting expired messages from {}: {:?}",
                                coll_name, e
                            );
                        }
                    }
                }

                // 移除太旧的集合(超过保留期限1个月以上)
                // 这不是直接删除集合，只是从活跃列表中移除
                let outdated_threshold = now
                    .checked_sub_months(Months::new((period / 30 + 2) as u32))
                    .unwrap_or(now)
                    .format("%Y_%m")
                    .to_string();

                let mut to_remove = Vec::new();
                for coll_name in collections.iter() {
                    // 解析集合名称中的年月部分
                    let parts: Vec<&str> = coll_name.split('_').collect();
                    if parts.len() >= 3 {
                        let year_month = format!("{}_{}", parts[2], parts[3]);
                        if year_month < outdated_threshold {
                            to_remove.push(coll_name.clone());
                        }
                    }
                }

                // 从跟踪列表中移除过期集合
                if !to_remove.is_empty() {
                    let mut write_collections = active_collections.write().await;
                    for coll_name in to_remove {
                        write_collections.remove(&coll_name);
                        info!("Removed outdated collection {} from tracking", coll_name);
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;
    use std::time::{SystemTime, UNIX_EPOCH};

    use abi::message::{MsgType, PlatformType};
    use utils::mongodb_tester::MongoDbTester;

    use super::*;

    struct TestConfig {
        box_: HybridShardedMsgBox,
        _tdb: MongoDbTester,
    }

    impl Deref for TestConfig {
        type Target = HybridShardedMsgBox;
        fn deref(&self) -> &Self::Target {
            &self.box_
        }
    }

    impl TestConfig {
        pub async fn new() -> Self {
            let config = Config::load("../config.yml").unwrap();
            let tdb = MongoDbTester::new(
                &config.db.mongodb.host,
                config.db.mongodb.port,
                &config.db.mongodb.user,
                &config.db.mongodb.password,
            )
            .await;
            let client = Client::with_uri_str(format!("mongodb://{}:{}", tdb.host, tdb.port))
                .await
                .unwrap();

            let msg_box = HybridShardedMsgBox::new(client, tdb.dbname.clone(), 10).await;
            Self {
                box_: msg_box,
                _tdb: tdb,
            }
        }
    }

    #[tokio::test]
    async fn sharded_insert_and_get_works() {
        let msg_box = TestConfig::new().await;
        let msg_id = format!("test_{}", get_timestamp_ms());
        let msg = get_test_msg(msg_id.clone());

        // 保存消息
        msg_box.save_message(&msg).await.unwrap();

        // 获取消息
        let retrieved_msg = msg_box.get_message(&msg_id).await.unwrap();
        assert!(retrieved_msg.is_some());
        assert_eq!(retrieved_msg.unwrap().server_id, msg_id);
    }

    #[tokio::test]
    async fn sharded_insert_and_delete_works() {
        let msg_box = TestConfig::new().await;
        let msg_id = format!("test_{}", get_timestamp_ms());
        let msg = get_test_msg(msg_id.clone());

        // 保存消息
        msg_box.save_message(&msg).await.unwrap();

        // 删除消息
        msg_box.delete_message(&msg_id).await.unwrap();

        // 检查消息已删除
        let retrieved_msg = msg_box.get_message(&msg_id).await.unwrap();
        assert!(retrieved_msg.is_none());
    }

    #[tokio::test]
    async fn sharded_query_messages_works() {
        let msg_box = TestConfig::new().await;
        let user_id = format!("user_{}", get_timestamp_ms());
        let base_time = Utc::now().timestamp();

        // 创建10条测试消息
        for i in 0..10 {
            let msg_id = format!("test_msg_{}_{}", user_id, i);
            let mut msg = get_test_msg(msg_id);
            msg.receiver_id = user_id.clone();
            msg.seq = i;
            msg.send_time = base_time + i;

            msg_box.save_message(&msg).await.unwrap();
        }

        // 查询部分消息
        let messages = msg_box.get_msgs(&user_id, 0, 0, 3, 7).await.unwrap();
        assert!(messages.len() > 0); // 确认有消息返回

        // 过滤出接收消息并按序列号排序
        let mut rec_messages: Vec<&Msg> = messages
            .iter()
            .filter(|msg| msg.seq >= 3 && msg.seq <= 7)
            .collect();
        rec_messages.sort_by_key(|msg| msg.seq);

        // 验证返回了预期的5个消息序列号
        let expected_seqs: Vec<i64> = (3..=7).collect();
        let actual_seqs: Vec<i64> = rec_messages.iter().map(|msg| msg.seq).collect();
        assert_eq!(actual_seqs, expected_seqs);
    }

    #[tokio::test]
    async fn sharded_group_message_works() {
        let msg_box = TestConfig::new().await;
        let base_id = format!("group_test_{}", get_timestamp_ms());
        let group_id = format!("group_{}", base_id);
        let sender_id = format!("sender_{}", base_id);

        // 创建群组消息
        let msg_id = format!("group_msg_{}", base_id);
        let mut msg = get_test_msg(msg_id.clone());
        msg.sender_id = sender_id.clone();
        msg.group_id = group_id.clone();
        msg.msg_type = MsgType::GroupMsg as i32;
        msg.send_seq = 100;

        // 群组成员
        let members = vec![
            GroupMemSeq {
                mem_id: format!("member_1_{}", base_id),
                cur_seq: 1001,
                max_seq: 1001,
                need_update: false,
            },
            GroupMemSeq {
                mem_id: format!("member_2_{}", base_id),
                cur_seq: 2001,
                max_seq: 2001,
                need_update: false,
            },
        ];

        // 保存群组消息
        msg_box
            .save_group_msg(msg.clone(), members.clone())
            .await
            .unwrap();

        // 检查发送者消息
        let sender_msgs = msg_box.get_msgs(&sender_id, 0, 1000, 0, 0).await.unwrap();
        assert!(!sender_msgs.is_empty());

        // 检查接收者消息
        let member1_msgs = msg_box
            .get_msgs(&members[0].mem_id, 0, 0, 1000, 1002)
            .await
            .unwrap();
        assert!(!member1_msgs.is_empty());
        assert_eq!(member1_msgs[0].seq, 1001);

        let member2_msgs = msg_box
            .get_msgs(&members[1].mem_id, 0, 0, 2000, 2002)
            .await
            .unwrap();
        assert!(!member2_msgs.is_empty());
        assert_eq!(member2_msgs[0].seq, 2001);
    }

    #[tokio::test]
    async fn sharded_message_read_status_works() {
        let msg_box = TestConfig::new().await;
        let user_id = format!("read_test_{}", get_timestamp_ms());

        // 创建5条测试消息
        let mut seq_list = Vec::new();
        for i in 0..5 {
            let msg_id = format!("read_msg_{}_{}", user_id, i);
            let mut msg = get_test_msg(msg_id);
            msg.receiver_id = user_id.clone();
            msg.seq = 100 + i;
            msg.is_read = false;
            seq_list.push(100 + i);

            msg_box.save_message(&msg).await.unwrap();
        }

        // 标记部分消息为已读 (前3条)
        msg_box.msg_read(&user_id, &seq_list[0..3]).await.unwrap();

        // 验证消息状态
        let messages = msg_box.get_msgs(&user_id, 0, 0, 100, 104).await.unwrap();

        // 按序列号检查每条消息的读取状态
        let read_status: HashMap<i64, bool> = messages
            .iter()
            .filter(|msg| msg.seq >= 100 && msg.seq <= 104)
            .map(|msg| (msg.seq, msg.is_read))
            .collect();

        // 验证所有5条消息都在返回结果中
        assert_eq!(read_status.len(), 5);

        // 检查前3条已读
        for seq in 100..=102 {
            assert!(read_status.get(&seq).unwrap(), "消息 seq={} 应该已读", seq);
        }

        // 检查后2条未读
        for seq in 103..=104 {
            assert!(!read_status.get(&seq).unwrap(), "消息 seq={} 应该未读", seq);
        }
    }

    // 辅助函数：获取毫秒级时间戳
    fn get_timestamp_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    // 辅助函数：创建测试消息
    fn get_test_msg(msg_id: String) -> Msg {
        Msg {
            client_id: format!("client_{}", get_timestamp_ms()),
            server_id: msg_id,
            create_time: 0,
            send_time: Utc::now().timestamp(),
            content_type: 0,
            content: "test content".to_string().into_bytes(),
            sender_id: format!("sender_{}", get_timestamp_ms()),
            receiver_id: format!("receiver_{}", get_timestamp_ms()),
            seq: 0,
            send_seq: 0,
            msg_type: MsgType::SingleMsg as i32,
            is_read: false,
            platform: PlatformType::Mobile as i32,
            group_id: "".to_string(),
            avatar: "".to_string(),
            nickname: "".to_string(),
            related_msg_id: None,
        }
    }
}
