use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "spoterm",
    version,
    about = "Spotify を操作する CLI（spotifyd 連携）"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Spotify にログイン（PKCE 認証）
    Login,
    /// 再生中の曲を表示（Now Playing）
    Status,
    /// 曲/アルバム/アーティストを検索
    Search {
        /// 検索キーワード（スペース区切りで複数語可）
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// 再生（クエリ指定で検索して再生、無指定で再開）
    Play { query: Vec<String> },
    /// 一時停止
    Pause,
    /// 次の曲へ
    Next,
    /// 前の曲へ
    Prev,
    /// 再生/一時停止のトグル
    Toggle,
    /// 音量を設定 (0-100)
    Vol {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        level: u8,
    },
    /// 利用可能なデバイス一覧（spotifyd を含む）
    Devices,
    /// デバイスを選択して再生を移す
    Device {
        #[command(subcommand)]
        action: DeviceAction,
    },
    /// プレイリスト操作
    Playlist {
        #[command(subcommand)]
        action: PlaylistAction,
    },
    /// ライブラリ（保存済みトラック/アルバム）
    Lib,
}

#[derive(Subcommand, Debug)]
pub enum DeviceAction {
    /// 指定名のデバイスへ再生を移す
    Use {
        #[arg(required = true)]
        name: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum PlaylistAction {
    /// プレイリスト一覧
    Ls,
    /// プレイリストを名前で再生
    Play {
        #[arg(required = true)]
        name: Vec<String>,
    },
}
