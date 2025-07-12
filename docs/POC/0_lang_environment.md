---
title: STEP 0 言語関連の環境設定
version: alpha
marp: true
---

## Elm

``` zsh
brew install pnpm
brew install elm
```

## Roc

[Apple Sillicon - Hot to install Roc.](https://www.roc-lang.org/install/macos_apple_silicon)

### Download the latest roc alpha release using the terminal

``` zsh
curl -OL https://github.com/roc-lang/roc/releases/download/alpha3-rolling/roc-macos_apple_silicon-alpha3-rolling.tar.gz
```

### Untar the archive

``` zsh
tar xf roc-macos_apple_silicon-alpha3-rolling.tar.gz
cd roc_night<TAB TO AUTOCOMPLETE>
```

### Install zstd

``` zsh
brew install zstd
```

## To be able to run the roc command anywhere on your system; add the line below to your shell startup script (.profile, .zshrc, ...)

``` zsh
export PATH=$PATH:~/path/to/roc_nightly-macos_apple_silicon-<VERSION>
```

Check everything worked by executing roc version

## Download and run hello world

``` zsh
curl -OL https://raw.githubusercontent.com/roc-lang/examples/refs/heads/main/examples/HelloWorld/main.roc
roc main.roc
```

## Rust

``` zsh
# 初回インストール
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 更新
rustup update
rustc --version

# tauri cliのインストール
cargo install tauri-cli --version "^2.0.0" --locked   
```

### tauri.conf.jsonについて

この設定ファイル内で`../elm-ui/dist/`のように１個上の階層に`elm-ui`があるかのように書かれているのは、
`pnpm tauri dev`コマンドが`apps/tauri-shell/tauri-src/`ではなく`apps/tauri-shell`

``` zsh
$ pnpm tauri dev

> tauri-shell@ tauri /Volumes/CODE/00_Development/000-proj/light-bringer/apps/tauri-shell
> tauri dev
```
