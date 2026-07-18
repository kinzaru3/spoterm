//! 読み取り系サブコマンドの実装（Phase 3）。各コマンドは `auth::authed_client` を入口に
//! 認証済みクライアントを取得し、Web API を叩いて結果を表示する。

pub mod devices;
pub mod search;
pub mod status;
