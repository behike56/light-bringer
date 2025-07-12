---
title: STEP 1 ローカル開発の方法
version: alpha
marp: true
---

## 動作確認方法

### 1. elm-ui – the front-end

``` zsh
cd apps/elm-ui
pnpm i         # installs elm + elm-live
pnpm run dev   # opens the browser on localhost:8000 (elm-live default)
```

### 2. roc-api – the back-end prototype

``` zsh
export ROC_BASIC_WEBSERVER_PORT=4000
cd apps/roc-api
roc run src/backend.roc -- --output ./kg-backend
```

### 3. tauri-shell – the desktop wrapper

``` zsh
cd apps/tauri-shell/src-tauri
pnpm i           # installs @tauri-apps/cli
pnpm tauri dev   # live-reload desktop app (expects elm-ui dev server on :5173)
pnpm tauri build # production build – bundles elm-ui/dist into the binary
```

### Neo4j container

``` zsh
docker compose -f docker/neo4j-compose.yml up -d
```

## Dev workflow

``` zsh
# terminal 1 – Elm UI
cd apps/elm-ui && pnpm run dev

# terminal 2 – Roc API
cd apps/roc-api && roc run src/backend.roc

# terminal 3 – Desktop wrapper
cd apps/tauri-shell && pnpm tauri dev
```
