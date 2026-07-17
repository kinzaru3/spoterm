# spoterm 開発コンテナ
# ホスト(mac)を汚さず Rust 開発を行う。spotifyd はホスト側で起動し、
# 本コンテナ内の spoterm とは Spotify クラウド経由で連携する。
FROM rust:1-bookworm

# ビルド依存 + コンテナ内で作業する場合のエディタ(vim)と dev ツール
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl ca-certificates pkg-config libssl-dev \
    vim ripgrep fd-find unzip less \
    && rm -rf /var/lib/apt/lists/*

# Rust 開発コンポーネント
RUN rustup component add rust-analyzer clippy rustfmt

# Debian では fd が `fdfind`。config が `fd` を期待する場合に備え symlink
RUN ln -s "$(which fdfind)" /usr/local/bin/fd

# login シェル(/etc/profile)で PATH が上書きされ cargo が外れるのを防ぐ
RUN printf 'export PATH="%s/bin:$PATH"\n' "$CARGO_HOME" > /etc/profile.d/cargo.sh

WORKDIR /workspace
