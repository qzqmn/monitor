# VPS Monitor

自架、Rust 編寫的輕量多機監控（中央端 + Agent）。

## 功能摘要

- 卡片式總覽：CPU / 記憶體 / 硬碟 / 網速 / 累計流量 / 在線 / 三網延遲
- 累計流量由中央端處理，設備重開機不丟失
- 可設定流量開始日、上限與告警百分比
- 國旗由 Agent 自動 GeoIP（ip-api.com）
- 通知：Telegram（自填 Token + Chat ID）、Webhook（填 URL 即可）
- 告警：離線、CPU、記憶體、流量百分比
- 管理後台需 ADMIN_SECRET

## 目錄結構

```
vps-monitor/
├── server/          # 中央端
├── agent/           # Agent 原始碼 + systemd / Dockerfile
├── docs/
│   ├── DEPLOY.md    # 部署流程
│   └── USER_MANUAL.md  # 使用說明
├── docker-compose.yml
└── data/            # SQLite 資料（執行後產生）
```

## 快速開始

1. 讀 docs/DEPLOY.md 部署中央端（Docker）
2. 在各機器部署 Agent（推薦 binary + systemd）
3. 讀 docs/USER_MANUAL.md 使用管理後台與通知
