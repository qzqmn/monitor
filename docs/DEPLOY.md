# VPS Monitor 部署手冊

## 一、架構說明

| 角色 | 建議部署方式 | 說明 |
|------|--------------|------|
| **中央端（Server）** | Docker Compose | 一台穩定有公網 IP 的 VPS |
| **Agent** | Docker Compose | 每台被監控機器；統一用 Docker，不再提供 systemd/binary 部署 |

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

資料保存在 `./data/monitor.db`，裡面含管理員密碼雜湊、每台機器的專屬密鑰與歷史流量，建議定期備份這個檔案（例如 cron 排程 `cp` 到別處，或連同整台機器做快照）。

> CI 現在除了 `:latest`，每次建置也會多打一個 `:sha-<git commit 前7碼>` 的版本 tag。想固定用某個已驗證版本、或新版本有問題要回滾，把 `docker-compose.yml` 裡的 `image:` 從 `:latest` 改成該版本的 tag 再 `docker compose up -d` 即可。
>
> **怎麼知道哪個 sha tag 比較新**：sha 本身只是 commit 的雜湊值，不像 `v1`/`v2` 那樣天生就看得出先後順序，但有兩個辦法對照：(1) GHCR 的 packages 頁面，每個 tag 旁邊都有「幾天前推送」的時間戳，當下 `:latest` 指到哪個 sha，那個 sha 旁邊也會多一個「Latest」標籤；(2) 這個 sha 本來就是一個真實的 git commit，`git log --oneline` 或 GitHub 上的 commit 紀錄可以直接查到它在哪個時間點、改了什麼。
>
> **`:latest` 還能不能用**：能，行為沒變，`docker-compose.yml` 預設也還是用它。中央端升級後，admin.html「新增機器」產生的 Agent compose 片段，映像 tag 已經改成自動抓目前中央端的 `sha-xxxxxxx`、不是永遠釘死 `:latest`，確保新裝的 Agent 跟中央端是同一次建置、不會有相容性落差；已經在跑的 Agent 不會自動跟著換版本，想一起升級就自己到那台機器 `docker compose pull && docker compose up -d`。

---

## 三、Agent 部署（被監控機器）

### 0. 先在後台「新增機器」登記（每台都要做這一步）

打開 `http://中央端IP:8080/admin.html` 登入後，最上面「➕ 新增機器」填一個 id（例如 `oracle-tokyo`）和顯示名稱，按「產生憑證」。畫面會直接秀出一份**填好值、可以直接存檔用的 `docker-compose.yml`**（不用再另外準備 `.env`），裡面已經包含這台專屬的 `MONITOR_URL`/`AGENT_ID`/`REPORT_SECRET`——**每台機器都要各自新增、各自拿一組不一樣的密鑰**，不是像舊版那樣全部機器共用一組。這組密鑰之後還能在機器管理列表裡點「顯示/複製」再看一次，忘記存也沒關係；真的洩漏了就點「重新產生」，只有那一台需要重新設定。

沒有先在這裡新增就直接啟動 Agent，中央端會直接拒絕上報（回應 403 unknown agent id）。

### 1. 部署 Agent（Docker）

把後台產生的內容存成該台機器上的 `docker-compose.yml`，然後：

```bash
docker compose up -d
docker compose logs -f
```

看到 `reported cpu=...` 且中央端網頁出現卡片即成功。

> `network_mode: host` 較容易正確統計網卡流量；若該機器環境不允許 host network（少見），可以改成橋接模式，但流量/連線數等統計可能不準。
>
> 如果你 fork 了本專案、image 不是推到 `qzqmn` 這個 GHCR 帳號，記得把 compose 裡 `image:` 那行的帳號改掉，或用 `cp agent/.env.example agent/.env` 那份帶 `GHCR_OWNER` 變數的範本自行組裝。

### 2. 多台機器

每一台都要先在後台「新增機器」各自登記，`AGENT_ID` 對應登記時的 id、`REPORT_SECRET` 是那台自己的專屬密鑰——**每台都不一樣**，不能像舊版那樣共用一組。

### 3. 機器不方便跑 Docker 怎麼辦

本專案目前只維護 Docker 這一種 Agent 部署方式，沒有現成的 systemd unit 或發行版可用。如果某台機器（例如資源很小的單板機、部分手機 postmarketOS 環境）跑 Docker 本身就有困難或不穩定，你可以：

- 從 `agent/` 目錄手動 `cd agent && cargo build --release` 編出對應架構的二進位檔，自己寫開機腳本執行（環境變數同 `agent/.env.example`），只是這條路徑不在本專案的官方文件/CI 驗證範圍內，之後行為變動不會特別遷就它；
- 或先確認清楚該機器上 Docker 本身能否正常跑（有些精簡系統對容器支援不完整，會在拉取/解壓映像時卡住或當機），排除掉之後再上 Agent。

---

## 四、NAS / pmOS 注意

- **NAS**：多數支援 Docker，直接照上面「三、Agent 部署」用 Docker 即可。
- **pmOS / 手機類設備**：部分精簡系統的 Docker 支援不完整或資源吃緊，實際跑之前建議先確認 Docker 本身（`docker run hello-world` 之類）在該機器上能穩定跑，不會卡住或讓系統當機；若不行，參考上一節「機器不方便跑 Docker 怎麼辦」自行編譯二進位檔執行。

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
