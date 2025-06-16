use chrono::Utc;
use clap::{Parser, ValueEnum};
use mongodb::Client;
use std::time::{Duration, Instant};

use abi::config::Config;
use abi::message::{GroupMemSeq, Msg, MsgType, PlatformType};
use db::message::MsgRecBoxRepo;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TestMode {
    /// 测试单聊消息
    Single,
    /// 测试群聊消息
    Group,
    /// 混合单聊和群聊消息
    Mixed,
}

#[derive(Debug, Parser)]
#[command(author, version, about = "MongoDB分片消息存储性能测试工具")]
struct Args {
    /// 测试模式
    #[arg(long, value_enum, default_value_t = TestMode::Mixed)]
    mode: TestMode,

    /// 测试消息数量
    #[arg(long, default_value_t = 10000)]
    count: usize,

    /// 用户分片数量
    #[arg(long, default_value_t = 10)]
    shards: u8,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 加载配置
    let config = Config::load("config.yml")?;
    let args = Args::parse();

    println!("启动分片消息存储性能测试");
    println!("测试模式: {:?}", args.mode);
    println!("消息数量: {}", args.count);
    println!("用户分片: {}", args.shards);

    // 创建临时测试数据库名称
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let test_db_name = format!("bench_test_{}", timestamp);
    println!("使用临时测试数据库: {}", test_db_name);

    // 连接MongoDB
    let client = Client::with_uri_str(config.db.mongodb.url()).await?;

    // 替换配置中的数据库名称为临时数据库
    let mut tmp_config = config.clone();
    tmp_config.db.mongodb.database = test_db_name.clone();

    // 创建消息仓库（通过接口而非具体实现）
    let message_box: Arc<dyn MsgRecBoxRepo> = if let Some(true) = tmp_config.db.mongodb.use_sharding
    {
        // 确保使用指定的分片数
        tmp_config.db.mongodb.user_shards = Some(args.shards as u32);
        db::msg_rec_box_repo(&tmp_config).await
    } else {
        // 如果未启用分片，强制启用它用于测试
        tmp_config.db.mongodb.use_sharding = Some(true);
        tmp_config.db.mongodb.user_shards = Some(args.shards as u32);
        db::msg_rec_box_repo(&tmp_config).await
    };

    // 根据测试模式执行测试
    let result = match args.mode {
        TestMode::Single => benchmark_single_messages(&message_box, args.count).await,
        TestMode::Group => benchmark_group_messages(&message_box, args.count).await,
        TestMode::Mixed => benchmark_mixed_messages(&message_box, args.count).await,
    };

    // 无论测试成功还是失败，都删除测试数据库
    println!("\n清理测试数据库: {}", test_db_name);
    match client.database(&test_db_name).drop(None).await {
        Ok(_) => println!("测试数据库已删除"),
        Err(e) => println!("删除数据库失败: {:?}", e),
    }

    // 返回测试结果
    result
}

async fn benchmark_single_messages(
    message_box: &Arc<dyn MsgRecBoxRepo>,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n开始单聊消息性能测试 ({} 条消息)...", count);

    let start = Instant::now();
    let base_time = Utc::now().timestamp();

    // 使用多个接收者分散消息，模拟真实场景
    let receivers = generate_user_ids(20, "recv");

    for i in 0..count {
        let recv_index = i % receivers.len();
        let msg = create_test_message(
            format!("s_{}", i),
            format!("sender_{}", i % 10),
            receivers[recv_index].clone(),
            base_time + i as i64,
            i as i64,
            MsgType::SingleMsg,
        );

        message_box.save_message(&msg).await?;

        if i % 1000 == 0 && i > 0 {
            println!("已处理 {} 条消息, 耗时: {:?}", i, start.elapsed());
        }
    }

    let duration = start.elapsed();
    print_benchmark_results(count, duration);

    Ok(())
}

async fn benchmark_group_messages(
    message_box: &Arc<dyn MsgRecBoxRepo>,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n开始群聊消息性能测试 ({} 条消息)...", count);

    let start = Instant::now();
    let base_time = Utc::now().timestamp();

    // 创建模拟群组和成员
    let groups = generate_group_ids(5);
    let members_per_group = 10;

    for i in 0..count {
        let group_index = i % groups.len();
        let _group_id = &groups[group_index];
        let sender_id = format!("sender_{}", i % 10);

        let msg = create_test_message(
            format!("g_{}", i),
            sender_id.clone(),
            "group".to_string(), // 会被成员ID覆盖
            base_time + i as i64,
            i as i64,
            MsgType::GroupMsg,
        );

        // 为每个群组成员创建接收序列
        let members = (0..members_per_group)
            .map(|j| GroupMemSeq {
                mem_id: format!("mem_{}_{}", group_index, j),
                cur_seq: 1000 + j as i64,
                max_seq: 1000 + j as i64,
                need_update: false,
            })
            .collect();

        message_box.save_group_msg(msg, members).await?;

        if i % 100 == 0 && i > 0 {
            println!("已处理 {} 条群消息, 耗时: {:?}", i, start.elapsed());
        }
    }

    let duration = start.elapsed();
    print_benchmark_results(count, duration);

    Ok(())
}

async fn benchmark_mixed_messages(
    message_box: &Arc<dyn MsgRecBoxRepo>,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n开始混合消息性能测试 ({} 条消息)...", count);

    // 70%单聊, 30%群聊
    let single_count = count * 7 / 10;
    let group_count = count - single_count;

    println!("单聊消息: {}, 群聊消息: {}", single_count, group_count);

    let start = Instant::now();

    // 顺序执行单聊和群聊测试，避免Send trait错误
    benchmark_single_messages(message_box, single_count).await?;
    benchmark_group_messages(message_box, group_count).await?;

    let duration = start.elapsed();
    println!("\n混合测试总结:");
    print_benchmark_results(count, duration);

    Ok(())
}

fn generate_user_ids(count: usize, prefix: &str) -> Vec<String> {
    (0..count).map(|i| format!("{}_{}", prefix, i)).collect()
}

fn generate_group_ids(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("group_{}", i)).collect()
}

fn create_test_message(
    id: String,
    sender_id: String,
    receiver_id: String,
    time: i64,
    seq: i64,
    msg_type: MsgType,
) -> Msg {
    Msg {
        client_id: format!("client_{}", id),
        server_id: format!("server_{}", id),
        create_time: time - 1,
        send_time: time,
        content_type: 0,
        content: format!("Test content for message {}", id).into_bytes(),
        sender_id,
        receiver_id,
        seq,
        send_seq: seq + 1000,
        msg_type: msg_type as i32,
        is_read: false,
        platform: PlatformType::Mobile as i32,
        group_id: if msg_type == MsgType::GroupMsg {
            format!("group_{}", id)
        } else {
            "".to_string()
        },
        avatar: "".to_string(),
        nickname: "".to_string(),
        related_msg_id: None,
    }
}

fn print_benchmark_results(count: usize, duration: Duration) {
    let total_seconds = duration.as_secs_f64();
    let msgs_per_second = count as f64 / total_seconds;

    println!("总消息数: {}", count);
    println!("总耗时: {:.2}秒", total_seconds);
    println!("每秒消息数: {:.2}", msgs_per_second);
    println!(
        "平均每条消息耗时: {:.2}微秒",
        duration.as_micros() as f64 / count as f64
    );
}
