# Polymarket 套利引擎 (Rust 版)

[English](README.md) | [简体中文](README_CN.md)

> [!TIP]
> **开源不易，如果这个项目启发了你，或者让你获得了收益，欢迎请我喝杯咖啡！☕**
> 
*   **捐赠地址 (BNB Chain):** `0xb5cac4ecb1168053bba4f725b92a423ab48d7018`
*   **请我喝咖啡：** ![Donation QR](./donation_qr.png)
>
> **“This repo proves why 99% of X influencers selling Polymarket arbitrage strategies are pure BS.”**
> （这个仓库证明了为什么推特上99%卖套利策略的博主都是在扯淡。）
>
> **“If this Rust engine doesn't tip the scales in your favor, then Go or Python versions aren't even worth the download—latency is the only metric that matters here.”**
> （如果这个 Rust 引擎都无法帮你套利成功，那么那些用 Go 或 Python 写的版本根本不值得下载——在毫秒级的博弈中，延迟是唯一的评判标准。）
>
> **“The code and strategy are now fully open. Consider this a final release; future updates are unlikely. Also, don't delude yourself: if dual-leg arbitrage fails, throwing in a third or fourth leg won't save you.”**
> （本软件的思路和代码已全部公开，后续大概率不会再更新。另外别抱幻想：如果双腿套利都失败，三腿或四腿套利更不可能成功。）

---
### 🚀 部署建议与基础设施

由于套利程序对速度有着极致的要求，本软件采用 **Rust** 构建以确保毫秒级的处理性能。为了获得最佳的执行效率，建议考虑以下部署策略：

*   **服务器位置**：Polymarket 的撮合服务器物理上位于 **英国伦敦**。为了降低网络往返时间 (RTT)，建议将机器人部署在靠近伦敦的数据中心。
*   **推荐区域**：强烈推荐使用 **爱尔兰 (Ireland)** 区域的服务器（如 AWS `eu-west-1`），这是目前延迟与合规性的最佳平衡点。
*   **地理限制 (Geo-blocking)**：请注意，**英国 IP 是被 Polymarket 拒绝访问的**。虽然服务器在伦敦，但你不能直接使用英国的服务器 IP 进行操作。
*   **合规性查询**：在选择部署区域前，请务必前往 [Polymarket 限制区域列表](https://docs.polymarket.com/api-reference/geoblock) 查询你所在区域的可用性。

## 关键技术成就

*   **零拷贝热路径 (Zero-Clone Hot Path)**：利用 `serde_json` 直接从字节流 (`&[u8]`) 动态解析 WebSocket 市场价格。消息传递采用移动语义 (`tokio::sync::mpsc`)，在关键路径上彻底消除了堆内存分配 and 内存拷贝。
*   **EIP-712 签名一致性**：严格验证类型化数据哈希和 `secp256k1` ECDSA 签名。生成的哈希和原始签名字节与 Python 官方 `eth-account` SDK 完美匹配，包括特定于 Polygon 的 `v + 27` 恢复位调整。
*   **可预测的并发模型**：舍弃了 Go 的 `sync.Map` 和指针锁，转而使用高度优化、无锁分片的 `DashMap` 来管理执行锁和冷却时间。分析状态使用 `std::sync::RwLock`，实现极速的并行读取。
*   **批量订单 FOK 执行**：在进行任何网络操作前，套利的两端 (YES 和 NO) 负载会同步构建完成，随后利用 Polymarket 的 `POST /orders` 批量接口统一发射，最大程度降低单腿成交风险。

## 套利原理

> **数学逻辑**：在二元结局市场（YES/NO）中，两种结局的价格总和在理论上应当等于结算金额（1.00 USDC）。
> 
> **获利机会**：当 `Price(YES) + Price(NO) < 1.00`（扣除交易手续费和滑点后）时，即存在套利空间。
> - **示例**：如果 `YES = $0.48`，`NO = $0.50`，总投入为 `$0.98`。
> - **结算**：无论结局如何，你手中必有一份头寸价值 `$1.00`，从而锁定 `$0.02` (2%) 的无风险利润。
> 
> **机器人的作用**：引擎实时监控 WebSocket 价格流，当发现双边总价低于 `MIN_PROFIT_THRESHOLD` 设定的阈值时，立即发起批量订单同步买入正反两面，捕捉这一瞬时的价差。

## 项目结构

```text
polymarket-rust/
├── Cargo.toml
└── src/
    ├── main.rs              # Tokio 主调度器 (多线程运行时)
    ├── config.rs            # 基于 .env 的核心配置加载器
    ├── models.rs            # 模型、市场和套利机会的结构定义
    ├── signer/mod.rs        # EIP-712 密码学与类型化数据生成
    ├── api/
    │   ├── clob.rs          # REST HTTP 请求与批量下单
    │   ├── gamma.rs         # GraphQL API 与市场发现逻辑
    │   └── websocket.rs     # WebSockets 零拷贝价格订阅与心跳
    ├── analyzer/mod.rs      # 套利阈值触发的数学逻辑
    ├── executor/mod.rs      # 构建并执行 FOK 双腿订单
    └── db/mod.rs            # 基于 sqlx 的异步 SQLite 持久化层
```

## 设置与配置

1. **安装 Rust** (如果尚未安装):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
2. **环境变量**:
   在 `polymarket-rust` 根目录下配置 `.env` 文件。程序会自动加载。请确保定义了以下关键参数：

   > [!NOTE]
   > **关于地址获取的说明 (针对邮箱登录用户):**
   > *注：自述文件作者仅使用过邮箱登录方式操作 Polymarket。*
   > *   **WALLET_ADDRESS**: 点击右上角自己的头像 -> 点击头像旁边的 **齿轮图标** -> 点击 "Private Key (私钥)" -> 点击 "Start Export (开始导出)"。会出现一段文字："Before you continue" 和 "By revealing the private key for"，这段文字下面一行就是你的 **WALLET_ADDRESS**。随后导出私钥时请务必妥善保存。
   > *   **FUNDER_ADDRESS**: 点击头像 -> 点击 **齿轮图标** -> 点击 "Developer Mode (开发者模式)" 即可获取。

   ```env
   # API 与 身份认证
   POLY_API_KEY=你的_key
   POLY_API_SECRET=你的_secret
   POLY_API_PASSPHRASE=你的_passphrase
   PRIVATE_KEY=你的私钥(不带0x)
   WALLET_ADDRESS=你的钱包_0x_地址
   FUNDER_ADDRESS=你的资金源地址 (可选，支持 Proxy 模式)
   SIGNATURE_TYPE=1                   # 1=EOA 签名, 0=默认
   
   # 交易策略阈值
   MIN_PROFIT_THRESHOLD=0.02          # 最小利润率 (例如 0.02 代表 2%)
   MAX_POSITION_SIZE=6                # 单腿最大投资额 (USDC)
   SLIPPAGE_PADDING=0.0015            # 价格滑点补偿 (例如 0.0015 代表单边 0.15%)
   LIQUIDITY_COEFFICIENT=0.5          # 允许吃掉的当前盘口深度比例 (0~1.0)
   MIN_SHARE_THRESHOLD=5              # 最少购买份额 (规避交易意义不大的小碎单)
   MIN_LIQUIDITY_USD=5000             # 过滤全场流动性(Liquidity)低于此值的市场
   MAX_LIQUIDITY_USD=50000            # 过滤超大型、竞争过于激烈的市场 (上限)
   MAX_DAYS_UNTIL_RESOLUTION=1        # 过滤距离结算时间超过此天数的市场
   
   > [!WARNING]
   > **风险提示**：请务必注意，如果两边的价格滑点补偿都吃满（例如单边 0.15%，双边合计约 0.3%），而你的 `MIN_PROFIT_THRESHOLD` 设置得过低（如低于 0.3%），最终结算可能会出现**亏损（负利润）**。虽然默认参数（2% 利润 vs 0.3% 总滑点）是安全的，但请根据个人的风险承受能力和市场波动情况谨慎设置。

   # 特殊过滤与性能
   EXCLUDE_CRYPTO_MINUTES=true       # 自动屏蔽 BTC/ETH 等带有极速陷阱的市场 (true/false)
   NUM_WS_CONNECTIONS=4               # 开启多少个并发 WebSocket 连接来订阅全场价格
   
   # 安全开关
   DRY_RUN=true                       # 设置为 false 开启实盘下单
   ```


## 常用命令

### 编译
编译生成针对当前机器优化的二进制文件：
```bash
cargo build --release
```

### 1. 交易引擎运行
**前台启动 (调试用)**:
```bash
cargo run --release
```

**后台启动 (推荐)**:
```bash
nohup ./target/release/polymarket-rust > bot.log 2>&1 &
```

**停止引擎**:
```bash
pkill polymarket-rust
# 如果 pkill 没反应，使用强力模式：
ps aux | grep polymarket-rust | grep -v grep | awk '{print $2}' | xargs kill -9
```

### 2. 看板程序运行
**启动看板**:
```bash
cd dashboard
source ../../venv/bin/activate  # 使用项目根目录的虚拟环境
nohup gunicorn -w 2 -b 127.0.0.1:8081 app:app > dashboard.log 2>&1 &
```

**停止看板**:
```bash
pkill gunicorn
```

### 3. 监控与日志
**查看交易日志**:
```bash
tail -f bot.log
```

**查看看板日志**:
```bash
tail -f dashboard/dashboard.log
```

## 日志审计 (SQLite)

引擎会将所有的 警报 (alerts)、成交记录 (trades)、单腿风险 (leg risks) 和 错过机会 (near misses) 直接持久化到本地的 SQLite 文件中，路径为 `data/rarb.db`。

由于其表结构与原 Go 版本完全一致，你可以直接沿用之前的看板程序或 Python 监控工具指向该目录读取数据。

### 关于 FOK 拒绝 (Rejects) 与 失败记录的特别说明
在看板或记录中，你可能会看到大量的 `FOK Rejects` 或 `Failures`，请**不要恐慌**，这是系统安全性的一种体现：
*   **审计日志而非风险**：这些记录属于“错过机会”或“被拒绝记录”，其含义是机器人发现了套利机会并尝试下单，但因竞争激烈或价格波动，未能在预设的滑点内成交。
*   **FOK (Fill-Or-Kill) 保护**：Rust 引擎强制使用 `FOK` 订单类型结合 `Batch Order`（批量下单）。这意味着：**要么 YES 和 NO 两条腿同时成交，要么整单直接被交易所撤销。**
*   **无单腿风险**：当你看到这类报错时，意味着**没有任何资金流出，也没有产生任何持仓**。它只是告诉你“我刚才试着打了一单，但没打中”。
### 状态诊断对照表 (如何判断单腿风险)
当你在日志或看板中看到 `YES:xxx|NO:xxx` 格式的记录时，可参考下表判断风险：

| 日志显示内容 | 风险等级 | 实际情况与建议 |
| :--- | :--- | :--- |
| `YES:FILLED\|NO:FILLED` | ✅ **安全** | 对冲成功，套利利润已锁定。 |
| `YES:REJECTED\|NO:REJECTED` | ✅ **安全** | FOK 触发。没买到，0 损失，0 风险。 |
| **`QUERY_ERR\|QUERY_ERR`** | 🟡 **注意** | **未知状态**。通常是网络抖动导致没查到结果。大概率没成交，但建议复核持仓。 |
| **`FILLED\|REJECTED`** | 🔴 **风险** | **单腿成交！** 一侧成交但另一侧失败，需手动平仓或补单。 |
| **`FILLED\|QUERY_ERR`** | 💀 **高危** | **疑似单腿！** 必须立刻去官网检查持仓，这是最危险的未知状态。 |

> [!TIP]
> 只要看到一侧是 `FILLED` 而另一侧**不是** `FILLED`，就说明可能存在单腿风险。如果两边都是 `QUERY_ERR` 或 `REJECTED`，通常是安全的。

## Python 签名基准 (`py_signer.py`)

该文件是使用 Python 官方 `eth-account` SDK 实现的 Polymarket EIP-712 签名逻辑参考实现。

- **用途**：作为“黄金标准（Golden Standard）”，用于确保 Rust 版本的签名器（`src/signer/mod.rs`）在算法上完全准确。签名逻辑中哪怕一个字节的错误（如字段顺序、类型转换）都会导致交易所返回 `INVALID_SIGNATURE` 错误。
- **使用方法**：
  1. 安装依赖：`pip install eth-account`
  2. 运行脚本：`python py_signer.py`
  3. 脚本会输出一个测试订单的 EIP-712 哈希和最终签名。
  4. 将此输出与 Rust 单元测试 `test_python_golden_standard` 中的结果进行比对，以验证 Rust 实现的正确性。
