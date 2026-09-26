# VPS Monitor 部署手冊

## 一、架構說明

| 角色 | 建議部署方式 | 說明 |
|------|--------------|------|
| **中央端（Server）** | Docker Compose | 一台穩定有公網 IP 的 VPS |
| **Agent** | 靜態 binary + systemd（推薦） | 每台被監控機器；也可選 Docker |

Agent **不建議**強制用 Docker：多一層開銷，且 NAS / pmOS 不一定方便跑容器。  
預設用「編譯好的 binary + systemd」最乾淨；需要時再給 Docker 範例。

---

## 二、中央端部署（主端）

### 1. 準備

- 一台 Linux VPS（建議 1 核 1G 以上）
- 已安裝 Docker（未安裝可執行：`curl -fsSL https://get.docker.com | sh`）

### 2. 上傳並解壓

把整個 `vps-monitor` 資料夾（或 zip）傳到伺服器，例如：

```bash
scp -r vps-monitor root@你的IP:/root/
# 或
scp vps-monitor.zip root@你的IP:/root/
ssh root@你的IP
cd /root && unzip vps-monitor.zip && cd vps-monitor
```

> 中央端映像由 GitHub Actions 自動建置並推到 `ghcr.io`，主機上**不需要**安裝 Rust 工具鏈，`docker compose` 只會拉現成映像。

### 3. 首次開啟，建立管理員帳號

現在不用先在檔案裡改密鑰了。啟動後直接開瀏覽器：

```
http://你的IP:8080/admin.html
```

第一次打開會是「建立管理員帳號」畫面，設定一組帳號密碼（密碼至少 8 個字元）即可，僅此一次——建立過後這個畫面就永久關閉，不能再重新註冊搶帳號。忘記密碼的話，只能到伺服器上用 `sqlite3 data/monitor.db "DELETE FROM admin_account;"` 清掉重來，沒有後門可以救。

想在架了 HTTPS 反向代理之後，讓登入用的 Cookie 加上 `Secure` 屬性，就把 `.env` 裡的 `COOKIE_SECURE` 改成 `true`；純 HTTP／Tailscale 內網存取請保持 `false`。

> 想自訂綁定位址/端口，或你的 GHCR 帳號不是 `qzqmn`，執行 `cp .env.example .env` 後改 `BIND_ADDR`/`PORT`/`GHCR_OWNER` 即可；不改也沒關係，`docker-compose.yml` 都有預設值。

### 4. 啟動

```bash
cd /root/vps-monitor
docker compose pull
docker compose up -d
```

完成後：

```bash
docker compose ps
docker compose logs -f
```

看到 `listening on 0.0.0.0:8080` 即成功。

> 如果你想改原始碼後自己建置測試（而不是用 ghcr 上的映像），把 `docker-compose.yml` 裡的 `image:` 那行註解掉、取消 `build:` 區塊的註解，再 `docker compose up -d --build`。

### 5. 訪問

- 監控頁：`http://你的IP:8080`
- 管理後台：`http://你的IP:8080/admin.html`

建議之後用 Nginx/Caddy 加域名與 HTTPS。

### 6. 常用指令

```bash
docker compose restart    # 重啟
docker compose logs -f    # 看日誌
docker compose down       # 停止
docker compose pull && docker compose up -d   # 更新到最新映像
```

資料保存在 `./data/monitor.db`。

---

## 三、Agent 部署（被監控機器）

### 0. 先在後台「新增機器」登記（每台都要做這一步）

打開 `http://中央端IP:8080/admin.html` 登入後，最上面「➕ 新增機器」填一個 id（例如 `oracle-tokyo`）和顯示名稱，按「產生憑證」。畫面會秀出一段完整的設定內容，包含這台專屬的 `REPORT_SECRET`——**每台機器都要各自新增、各自拿一組不一樣的密鑰**，不是像舊版那樣全部機器共用一組。這組密鑰之後還能在機器管理列表裡點「顯示/複製」再看一次，忘記存也沒關係；真的洩漏了就點「重新產生」，只有那一台需要重新設定。

沒有先在這裡新增就直接啟動 Agent，中央端會直接拒絕上報（回應 403 unknown agent id）。

### 方式 A：靜態 Binary + systemd（推薦）

#### 1. 在有 Rust 的機器編譯（或你的開發機）

```bash
# 進入專案
cd vps-monitor

# 編譯當前架構
cargo build -p vps-monitor-agent --release

# 交叉編譯 ARM64、glibc（例如 Oracle ARM VPS，跑一般發行版）
rustup target add aarch64-unknown-linux-gnu
cargo build -p vps-monitor-agent --release --target aarch64-unknown-linux-gnu

# 交叉編譯 ARM64、musl（postmarketOS / Alpine 系統要用這個，不是 -gnu；
# 建議用 `cross` 處理 linker，直接 rustup target add 通常編不過）
cargo install cross --git https://github.com/cross-rs/cross
cross build -p vps-monitor-agent --release --target aarch64-unknown-linux-musl
```

產出檔案：

- x86_64：`target/release/vps-monitor-agent`
- ARM64（glibc）：`target/aarch64-unknown-linux-gnu/release/vps-monitor-agent`
- ARM64（musl / pmOS）：`target/aarch64-unknown-linux-musl/release/vps-monitor-agent`

#### 2. 複製到被監控機器

```bash
scp target/release/vps-monitor-agent root@被監控IP:/usr/local/bin/
# ARM 則用對應路徑
ssh root@被監控IP
chmod +x /usr/local/bin/vps-monitor-agent
```

#### 3. 安裝 systemd 服務

```bash
# 編輯範例
nano /etc/systemd/system/vps-monitor-agent.service
```

內容參考 `agent/vps-monitor-agent.service`，至少改：

```ini
Environment=MONITOR_URL=http://中央端IP:8080
Environment=AGENT_ID=跟後台新增機器時填的 id 完全一致，例如 oracle-tokyo
Environment=AGENT_NAME=顯示名稱例如 Oracle-日本東京
Environment=REPORT_SECRET=後台新增機器時顯示的專屬密鑰
Environment=INTERVAL_SECS=20
```

然後：

```bash
systemctl daemon-reload
systemctl enable --now vps-monitor-agent
systemctl status vps-monitor-agent
journalctl -u vps-monitor-agent -f
```

看到 `reported cpu=...` 且中央端網頁出現卡片即成功。

#### 4. 多台機器

每一台都要先在後台「新增機器」各自登記，`AGENT_ID` 對應登記時的 id、`REPORT_SECRET` 是那台自己的專屬密鑰——**每台都不一樣**，不能像舊版那樣共用一組，`AGENT_NAME` 隨意。

---

### 方式 B：Agent 用 Docker（可選）

```bash
cd vps-monitor/agent
cp .env.example .env   # 填 MONITOR_URL / AGENT_ID（後台登記的 id）/ REPORT_SECRET（後台顯示的專屬密鑰）
docker compose pull
docker compose up -d
```

同樣是拉 ghcr 上 CI 建好的多架構映像，不用在被監控機器上裝 Rust 工具鏈。`network_mode: host` 較容易正確統計網卡流量；若不行可改橋接並接受流量統計略有偏差。

---

## 四、NAS / pmOS 注意

- **NAS**：若支援 Docker 可用方式 B；否則用對應架構的 static binary + 開機腳本或 systemd（若有）。
- **pmOS Mix 2S**：用 ARM64 binary，手動或用 openrc/systemd 開機啟動；需能出網訪問中央端與 ip-api.com（國旗）。

---

## 五、防火牆

中央端需放行 **8080**（或你改的端口）給需要看面板 / Agent 需要上報的來源。  
**建議只對 Tailscale 網段開放**，不要直接對公網開放明碼 HTTP：把 `docker-compose.yml` 的 port 映射改成綁定 Tailscale IP，例如 `"100.x.x.x:8080:8080"`，要對外公開再另外套 Caddy/Nginx 反向代理 + HTTPS。  
Agent 只需出站訪問中央端，一般不用開入站端口。

---

## 六、驗證清單

1. 中央端 `docker compose ps` 為 Up
2. 瀏覽器能打開主頁
3. 已經在後台「新增機器」登記過要監控的每一台
4. Agent 日誌有 `reported`
5. 主頁出現對應卡片，數值在更新
6. 管理後台登入後能改名 / 重設流量 / 存通知設定 / 新增與刪除機器
7. 點「發送測試」能收到 Telegram 或 Webhook
