use crate::Cache;
use abi::config::Config;
use abi::errors::Error;
use abi::message::GroupMemSeq;
use async_trait::async_trait;
use redis::AsyncCommands;

/// group members id prefix
const GROUP_MEMBERS_ID_PREFIX: &str = "group_members_id";

/// register code key
const REGISTER_CODE_KEY: &str = "register_code";

/// register code expire time
const REGISTER_CODE_EXPIRE: i64 = 300;

const USER_ONLINE_SET: &str = "user_online_set";

const DEFAULT_SEQ_STEP: i32 = 5000;

const EVALSHA: &str = "EVALSHA";

const CUR_SEQ_KEY: &str = "cur_seq";

const MAX_SEQ_KEY: &str = "max_seq";

const IS_LOADED: &str = "seq_need_load";

const SEQ_NO_NEED_LOAD: &str = "false";

#[derive(Debug)]
pub struct RedisCache {
    client: redis::Client,
    seq_step: i32,
    single_seq_exe_sha: String,
    group_seq_exe_sha: String,
}

impl RedisCache {
    #[allow(dead_code)]
    pub fn new(client: redis::Client) -> Self {
        let seq_exe_sha = Self::single_script_load(&client);
        let group_seq_exe_sha = Self::group_script_load(&client);
        let seq_step = DEFAULT_SEQ_STEP;
        Self {
            client,
            single_seq_exe_sha: seq_exe_sha,
            seq_step,
            group_seq_exe_sha,
        }
    }
    pub fn from_config(config: &Config) -> Self {
        // Intentionally use unwrap to ensure Redis connection at startup.
        // Program should panic if unable to connect to Redis, as it's critical for operation.
        let client = redis::Client::open(config.redis.url()).unwrap();
        // init redis
        let single_seq_exe_sha = Self::single_script_load(&client);
        let group_seq_exe_sha = Self::group_script_load(&client);
        let mut seq_step = DEFAULT_SEQ_STEP;
        if config.redis.seq_step != 0 {
            seq_step = config.redis.seq_step;
        }
        Self {
            client,
            seq_step,
            single_seq_exe_sha,
            group_seq_exe_sha,
        }
    }

    fn single_script_load(client: &redis::Client) -> String {
        let mut conn = client.get_connection().unwrap();

        let script = r#"
        local cur_seq = redis.call('HINCRBY', KEYS[1], 'cur_seq', 1)
        local max_seq = redis.call('HGET', KEYS[1], 'max_seq')
        local updated = false
        if max_seq == false then
            max_seq = tonumber(ARGV[1])
            redis.call('HSET', KEYS[1], 'max_seq', max_seq)
            end
        if tonumber(cur_seq) > tonumber(max_seq) then
            max_seq = tonumber(max_seq) + ARGV[1]
            redis.call('HSET', KEYS[1], 'max_seq', max_seq)
            updated = true
        end
        return {cur_seq, max_seq, updated}
        "#;
        redis::Script::new(script)
            .prepare_invoke()
            .load(&mut conn)
            .unwrap()
    }

    fn group_script_load(client: &redis::Client) -> String {
        let mut conn = client.get_connection().unwrap();

        let script = r#"
        local seq_step = tonumber(ARGV[1])
        local result = {}

        for i=2,#ARGV do
            local key = "seq:" .. ARGV[i]
            local cur_seq = redis.call('HINCRBY', key, 'cur_seq', 1)
            local max_seq = redis.call('HGET', key, 'max_seq')
            local updated = 0
            if max_seq == false then
                max_seq = seq_step
                redis.call('HSET', key, 'max_seq', max_seq)
            else
                max_seq = tonumber(max_seq)
            end
            if cur_seq > max_seq then
                max_seq = max_seq + seq_step
                redis.call('HSET', key, 'max_seq', max_seq)
                updated = 1
            end
            table.insert(result, {cur_seq, max_seq, updated})
        end

        return result
        "#;
        redis::Script::new(script)
            .prepare_invoke()
            .load(&mut conn)
            .unwrap()
    }
}

#[async_trait]
impl Cache for RedisCache {
    async fn check_seq_loaded(&self) -> Result<bool, Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;

        // redis.get::<K,U>() K is a key, U is a type of returned value
        let need_load = conn.get::<_, Option<String>>(IS_LOADED).await;
        match need_load {
            Ok(Some(value)) if value == SEQ_NO_NEED_LOAD => return Ok(false),
            _ => return Ok(true),
        }
    }

    async fn set_seq_loaded(&self) -> Result<(), Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.set(IS_LOADED, SEQ_NO_NEED_LOAD).await?;
        Ok(())
    }

    async fn set_seq(&self, max_seq: &[(String, i64, i64)]) -> Result<(), Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let mut pipe = redis::pipe();
        for (user_id, send_max_seq, rec_max_seq) in max_seq {
            let key = format!("send_seq:{}", user_id);
            pipe.hset(&key, CUR_SEQ_KEY, send_max_seq);
            pipe.hset(&key, MAX_SEQ_KEY, send_max_seq);
            let key = format!("seq:{}", user_id);
            pipe.hset(&key, CUR_SEQ_KEY, rec_max_seq);
            pipe.hset(&key, MAX_SEQ_KEY, rec_max_seq);
        }
        let _: () = pipe.query_async(&mut conn).await?;
        Ok(())
    }

    async fn set_send_seq(&self, max_seq: &[(String, i64)]) -> Result<(), Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let mut pipe = redis::pipe();
        for (user_id, max_seq) in max_seq {
            let key = format!("send_seq:{}", user_id);
            pipe.hset(&key, CUR_SEQ_KEY, max_seq);
            pipe.hset(&key, MAX_SEQ_KEY, max_seq);
        }
        let _: () = pipe.query_async(&mut conn).await?;
        Ok(())
    }

    async fn get_seq(&self, user_id: &str) -> Result<i64, Error> {
        // generate key
        let key = format!("seq:{}", user_id);

        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let seq: i64 = conn.hget(&key, CUR_SEQ_KEY).await.unwrap_or_default();
        Ok(seq)
    }

    async fn get_cur_seq(&self, user_id: &str) -> Result<(i64, i64), Error> {
        // generate key
        let key1 = format!("seq:{}", user_id);
        let key2 = format!("send_seq:{}", user_id);

        let mut conn = self.client.get_multiplexed_async_connection().await?;
        // let seq: i64 = conn.hget(&key, CUR_SEQ_KEY).await.unwrap_or_default();
        let (seq1, seq2): (i64, i64) = redis::pipe()
            .cmd("HGET")
            .arg(&key1)
            .arg(CUR_SEQ_KEY)
            .cmd("HGET")
            .arg(&key2)
            .arg(CUR_SEQ_KEY)
            .query_async(&mut conn)
            .await?;

        Ok((seq1, seq2))
    }

    async fn get_send_seq(&self, user_id: &str) -> Result<(i64, i64), Error> {
        // generate key
        let key = format!("send_seq:{}", user_id);

        let mut conn = self.client.get_multiplexed_async_connection().await?;
        // let seq: i64 = conn.hget(&key, CUR_SEQ_KEY).await.unwrap_or_default();
        // Ok(seq)
        let (cur_seq, max_seq): (Option<i64>, Option<i64>) = redis::pipe()
            .cmd("HGET")
            .arg(&key)
            .arg("cur_seq")
            .cmd("HGET")
            .arg(&key)
            .arg("max_seq")
            .query_async(&mut conn)
            .await?;

        // 处理默认值
        let cur_seq = cur_seq.unwrap_or_default();
        let max_seq = max_seq.unwrap_or_default();

        Ok((cur_seq, max_seq))
    }

    async fn increase_seq(&self, user_id: &str) -> Result<(i64, i64, bool), Error> {
        // generate key
        let key = format!("seq:{}", user_id);

        let mut conn = self.client.get_multiplexed_async_connection().await?;
        // increase seq
        let seq = redis::cmd(EVALSHA)
            .arg(&self.single_seq_exe_sha)
            .arg(1)
            .arg(&key)
            .arg(self.seq_step)
            .query_async(&mut conn)
            .await?;
        Ok(seq)
    }

    async fn incr_send_seq(&self, user_id: &str) -> Result<(i64, i64, bool), Error> {
        // generate key
        let key = format!("send_seq:{}", user_id);

        let mut conn = self.client.get_multiplexed_async_connection().await?;
        // increase seq
        let seq = redis::cmd(EVALSHA)
            .arg(&self.single_seq_exe_sha)
            .arg(1)
            .arg(&key)
            .arg(self.seq_step)
            .query_async(&mut conn)
            .await?;
        Ok(seq)
    }

    async fn incr_group_seq(&self, mut members: Vec<String>) -> Result<Vec<GroupMemSeq>, Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;

        let mut cmd = redis::cmd(EVALSHA);
        cmd.arg(&self.group_seq_exe_sha).arg(0).arg(self.seq_step);

        for member in members.iter() {
            cmd.arg(member);
        }

        let response: Vec<redis::Value> = cmd.query_async(&mut conn).await?;

        let mut seq = Vec::with_capacity(members.len());
        for item in response.into_iter() {
            if let redis::Value::Bulk(bulk_item) = item {
                if bulk_item.len() == 3 {
                    if let (
                        redis::Value::Int(cur_seq),
                        redis::Value::Int(max_seq),
                        redis::Value::Int(updated),
                    ) = (&bulk_item[0], &bulk_item[1], &bulk_item[2])
                    {
                        seq.push(GroupMemSeq::new(
                            members.remove(0),
                            *cur_seq,
                            *max_seq,
                            *updated != 0,
                        ));
                    }
                }
            }
        }
        Ok(seq)
    }

    /// the group members id in redis is a set, with group_members_id:group_id as key
    async fn query_group_members_id(&self, group_id: &str) -> Result<Vec<String>, Error> {
        // generate key
        let key = format!("{}:{}", GROUP_MEMBERS_ID_PREFIX, group_id);
        // query value from redis
        let mut conn = self.client.get_multiplexed_async_connection().await?;

        let result: Vec<String> = conn.smembers(&key).await?;
        Ok(result)
    }

    async fn save_group_members_id(
        &self,
        group_id: &str,
        members_id: Vec<String>,
    ) -> Result<(), Error> {
        let key = format!("{}:{}", GROUP_MEMBERS_ID_PREFIX, group_id);
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        // Add each member to the set for the group by redis pipe
        let mut pipe = redis::pipe();
        for member in members_id {
            pipe.sadd(&key, &member);
        }
        let _: () = pipe.query_async(&mut conn).await?;
        Ok(())
    }

    async fn add_group_member_id(&self, member_id: &str, group_id: &str) -> Result<(), Error> {
        let key = format!("{}:{}", GROUP_MEMBERS_ID_PREFIX, group_id);
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.sadd(&key, member_id).await?;
        Ok(())
    }

    async fn remove_group_member_id(&self, group_id: &str, member_id: &str) -> Result<(), Error> {
        let key = format!("{}:{}", GROUP_MEMBERS_ID_PREFIX, group_id);
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.srem(&key, member_id).await?;
        Ok(())
    }

    async fn remove_group_member_batch(
        &self,
        group_id: &str,
        member_id: &[&str],
    ) -> Result<(), Error> {
        let key = format!("{}:{}", GROUP_MEMBERS_ID_PREFIX, group_id);
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.srem(&key, member_id).await?;
        Ok(())
    }

    async fn del_group_members(&self, group_id: &str) -> Result<(), Error> {
        let key = format!("{}:{}", GROUP_MEMBERS_ID_PREFIX, group_id);
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.del(&key).await?;
        Ok(())
    }

    async fn save_register_code(&self, email: &str, code: &str) -> Result<(), Error> {
        // set the register code with 5 minutes expiration time
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        // use pipe to exec two commands
        let mut pipe = redis::pipe();
        let _: () = pipe
            .hset(REGISTER_CODE_KEY, email, code)
            .expire(REGISTER_CODE_KEY, REGISTER_CODE_EXPIRE)
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    async fn get_register_code(&self, email: &str) -> Result<Option<String>, Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let result = conn.hget(REGISTER_CODE_KEY, email).await?;
        Ok(result)
    }

    async fn del_register_code(&self, email: &str) -> Result<(), Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.hdel(REGISTER_CODE_KEY, email).await?;
        Ok(())
    }

    async fn user_login(&self, user_id: &str) -> Result<(), Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.sadd(USER_ONLINE_SET, user_id).await?;
        Ok(())
    }

    async fn user_logout(&self, user_id: &str) -> Result<(), Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.srem(USER_ONLINE_SET, user_id).await?;
        Ok(())
    }

    async fn online_count(&self) -> Result<i64, Error> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let result: i64 = conn.scard(USER_ONLINE_SET).await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi::config::Config;
    use std::ops::Deref;
    use std::thread;
    use tokio::runtime::Runtime;

    struct TestRedis {
        client: redis::Client,
        cache: RedisCache,
    }

    impl Deref for TestRedis {
        type Target = RedisCache;
        fn deref(&self) -> &Self::Target {
            &self.cache
        }
    }

    impl Drop for TestRedis {
        fn drop(&mut self) {
            let client = self.client.clone();
            thread::spawn(move || {
                Runtime::new().unwrap().block_on(async {
                    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
                    // let _: () to tell the compiler that the query_async method's return type is ()
                    let _: () = redis::cmd("FLUSHDB").query_async(&mut conn).await.unwrap();
                })
            })
            .join()
            .unwrap();
        }
    }
    impl TestRedis {
        fn new() -> Self {
            // use the 9 database for test
            let database = 9;
            Self::from_db(database)
        }

        // because of the tests are running in parallel,
        // we need to use different database,
        // in case of the flush db command will cause conflict in drop method
        fn from_db(db: u8) -> Self {
            let config = Config::load("../config.yml").unwrap();
            let url = format!("{}/{}", config.redis.url(), db);
            let client = redis::Client::open(url).unwrap();
            let cache = RedisCache::new(client.clone());
            TestRedis { client, cache }
        }
    }
    #[tokio::test]
    async fn test_increase_seq() {
        let user_id = "test";
        let cache = TestRedis::new();
        let seq = cache.increase_seq(user_id).await.unwrap();
        assert_eq!(seq, (1, DEFAULT_SEQ_STEP as i64, false));
    }

    #[tokio::test]
    async fn test_save_group_members_id() {
        let group_id = "test";
        let members_id = vec!["1".to_string(), "2".to_string()];
        let cache = TestRedis::new();
        let result = cache.save_group_members_id(group_id, members_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_query_group_members_id() {
        let group_id = "test";
        let members_id = vec!["1".to_string(), "2".to_string()];
        let db = 8;
        let cache = TestRedis::from_db(db);
        let result = cache.save_group_members_id(group_id, members_id).await;
        assert!(result.is_ok());
        let result = cache.query_group_members_id(group_id).await.unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains(&"1".to_string()));
        assert!(result.contains(&"2".to_string()));
    }

    #[tokio::test]
    async fn test_add_group_member_id() {
        let group_id = "test";
        let member_id = "1";
        let cache = TestRedis::new();
        let result = cache.add_group_member_id(member_id, group_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_remove_group_member_id() {
        let group_id = "test";
        let member_id = "1";
        let cache = TestRedis::new();
        let result = cache.add_group_member_id(member_id, group_id).await;
        assert!(result.is_ok());
        let result = cache.remove_group_member_id(group_id, member_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_del_group_members() {
        let group_id = "test";
        let members_id = vec!["1".to_string(), "2".to_string()];
        let cache = TestRedis::new();
        // need to add first
        let result = cache.save_group_members_id(group_id, members_id).await;
        assert!(result.is_ok());
        let result = cache.del_group_members(group_id).await;
        assert!(result.is_ok());
    }
}
