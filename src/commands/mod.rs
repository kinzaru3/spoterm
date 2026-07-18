//! サブコマンドの実装。各コマンドは `auth::authed_client` を入口に認証済みクライアントを
//! 取得し、Web API を叩いて結果を表示・操作する（読み取り系 Phase 3 / 再生系 Phase 4 /
//! プレイリスト・ライブラリ Phase 5）。

pub mod device;
pub mod devices;
pub mod lib;
pub mod playback;
pub mod playlist;
pub mod search;
pub mod status;

/// アクティブデバイスが必要な操作で失敗時に添えるヒント（playback / playlist で共用）。
pub(crate) const NEED_DEVICE_HINT: &str =
    "（アクティブなデバイスが必要です。`spoterm device use <name>` で選択してください）";
