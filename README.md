# VPS Monitor

自架、Rust 編寫的輕量多機監控（中央端 + Agent）。

## 功能摘要

- 卡片式總覽：CPU / 記憶體 / 硬碟 / 網速 / 累計流量 / 在線
- 點卡片看歷史趨勢圖（24 小時 / 7 天可切換）
- 累計流量由中央端處理，設備重開機不丟失
- 可設定流量開始日、上限、告警百分比與**計費方向**（合計/只算出站/只算入站，對應各家 VPS 商不同的計費規則）
- 國旗由 Agent 自動 GeoIP（ip-api.com）
- 通知：Telegram（自填 Token + Chat ID）、Webhook（填 URL 即可）
- 告警：離線、CPU、記憶體、流量百分比
- 管理後台網頁登入（帳密），每台機器各自一組專屬上報密鑰
- server / agent 都內建 Docker HEALTHCHECK

## 目錄結構

```
vps-monitor/
├── server/          # 中央端
├── agent/           # Agent 原始碼 + Dockerfile（純 Docker 部署）
├── docs/
│   ├── DEPLOY.md    # 部署流程
│   └── USER_MANUAL.md  # 使用說明
├── docker-compose.yml
└── data/            # SQLite 資料（執行後產生）
```

## 快速開始

1. 讀 docs/DEPLOY.md 部署中央端（Docker）
2. 在各機器部署 Agent（Docker，登記機器時後台會直接產生可用的 docker-compose.yml）
3. 讀 docs/USER_MANUAL.md 使用管理後台與通知
