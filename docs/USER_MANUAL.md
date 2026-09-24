# VPS Monitor 使用說明（工作手冊）

## 1. 監控主頁

網址：`http://中央端IP:8080`

每張卡片顯示一台機器：

- 名稱、國旗、城市
- CPU 使用率與 Load
- 記憶體 / 硬碟（進度條 + 數值）
- 即時上下行速率
- 累計流量（從「開始計數日」起，重開機由中央端自動接續）
- 若有設定流量上限，會顯示使用百分比進度條
- 在線時長
- 三網延遲與丟包（Agent 有上報時才有）
- 最後更新時間

頁面約每 10 秒自動刷新。主頁為**唯讀**，不能改設定。

---

## 2. 管理後台

網址：`http://中央端IP:8080/admin.html`

頂部先填 **ADMIN_SECRET**（與 docker-compose 裡相同），再操作。

### 2.1 重設流量開始日

1. 填寫機器 **ID**（與 Agent 的 `AGENT_ID` 一致，可在卡片或資料庫中確認）
2. 點「重設」  
→ 該機累計流量歸零，並記錄新的開始時間。

### 2.2 修改顯示名稱

1. 填寫機器 ID  
2. 填寫新名稱  
3. 點「修改」  

不會改 Agent 端的 ID，只改面板顯示名稱。

### 2.3 流量上限與告警百分比

1. 機器 ID  
2. 上限 GB（填 `0` 表示不限制）  
3. 告警百分比（例如 `80` 表示達到 80% 時通知）  
4. 點「更新」  

達到百分比時，若已開啟通知，會發送「流量告警」。

### 2.4 通知設定

| 項目 | 說明 |
|------|------|
| 啟用 Telegram | 勾選後需填 Bot Token、Chat ID |
| Bot Token | 向 @BotFather 建立 Bot 取得 |
| Chat ID | 個人或群組 ID（可用 @userinfobot 等查詢） |
| 啟用 Webhook | 勾選後填完整 URL，例如 `https://abc.com/alert` |
| 離線分鐘 | 超過此時間未上報即發離線告警（預設 5） |
| CPU / 記憶體閾值 | 超過則告警（預設 90%） |

點「儲存通知設定」後生效。  
點「發送測試」可驗證 Telegram / Webhook 是否通。

**Webhook 收到的 JSON 範例：**

```json
{
  "title": "流量告警",
  "message": "Oracle-東京 累計流量已達 85.0%（850.20 GB / 1000 GB）",
  "server_id": "oracle-tokyo",
  "server_name": "Oracle-日本東京",
  "type": "traffic",
  "level": "warning",
  "value": 85.0,
  "limit": 1000.0,
  "timestamp": "2026-09-20T12:00:00+00:00"
}
```

`type` 可能為：`offline` / `cpu` / `memory` / `traffic` / `test`。

---

## 3. 累計流量說明

- Agent 只上報系統當下的原始網卡計數器。  
- 中央端用「本次 − 上次」計算增量；若偵測到重開機（數值變小），會把本次開機後的流量接續加總。  
- 因此**重開機不會清空**面板上的累計（除非你手動「重設流量開始日」）。  
- 長時間離線期間的流量無法補回（所有此類方案共同限制）。

---

## 4. 國旗與地區

Agent 啟動時向 `ip-api.com` 查詢公網 IP 對應國家/城市並上報。  
若機器無外網或 API 失敗，國旗可能空白，不影響其他監控。

---

## 5. 告警觸發條件彙總

1. **離線**：超過「離線分鐘」未上報（背景每分鐘檢查），離線期間只通知一次，恢復上報後補發一則恢復通知
2. **CPU**：單次上報時 CPU ≥ 閾值
3. **記憶體**：使用率 ≥ 閾值
4. **流量**：`(cum_in + cum_out) / traffic_limit * 100 ≥ 設定百分比`

CPU / 記憶體 / 流量三種告警都有 30 分鐘冷卻：同一台機器同一種告警在冷卻時間內只會發一次，不會每次上報（預設 20 秒一次）都連環轟炸。

---

## 6. 安全建議

- `REPORT_SECRET`、`ADMIN_SECRET` 使用長隨機字串，放在 `.env`（不要提交進 git）
- 不要把管理後台暴露在無密碼的公網；管理 API 已改用 `Authorization: Bearer` 認證，但仍建議只從 Tailscale 或加反向代理 + HTTPS 存取
- 中央端映像改由 CI 自動建置並推到 ghcr.io，更新時 `docker compose pull && docker compose up -d` 即可，不需要在主機上重新編譯
- Agent 以普通權限執行即可（讀 `/proc` 等）；無需 root 執行命令通道

---

## 7. 故障排除

| 現象 | 檢查 |
|------|------|
| 主頁無卡片 | Agent 是否在跑、SECRET 是否一致、中央端日誌 |
| Agent 報 unauthorized | `REPORT_SECRET` 與中央端不一致 |
| 管理操作失敗 | `ADMIN_SECRET` 是否正確 |
| 無國旗 | Agent 能否訪問 ip-api.com |
| 流量不漲 | 是否剛重設；或 Agent 網卡統計權限 |
| 收不到 Telegram | Token/Chat ID、是否勾選啟用、點測試按鈕 |
| Webhook 無反應 | URL 是否可從中央端訪問、HTTPS 憑證 |

查看中央端日誌：

```bash
docker compose logs -f
```

查看 Agent（systemd）：

```bash
journalctl -u vps-monitor-agent -f
```
