//! カバーアート表示のサポート（Phase 6.6）。表示に使う画像 URL の選択（純粋関数）と、
//! 画像の HTTP 取得＋デコードを担う。端末への描画（プロトコル判定・レンダリング）は
//! `ratatui-image` の `Picker`/`StatefulProtocol` を使って `mod.rs` 側で行う。
//!
//! - 取得は保持している `reqwest::Client` を使い回す（接続プールを捨てない）。
//! - デコードは CPU バウンドなため `spawn_blocking` に逃がし、UI ループを止めない。

use std::io::Cursor;

use anyhow::{Context, Result, bail};
use image::{DynamicImage, ImageReader, Limits};

/// カバーアートとして狙う横幅（px）。端末表示には中サイズで十分。取得・デコードも軽い。
const TARGET_WIDTH: u32 = 300;
/// デコード時の最大画像寸法（px）。カバーアートは実際 640×640 程度。展開爆弾対策の余裕上限。
const MAX_IMAGE_DIM: u32 = 4096;

/// カバーアート URL の許可判定（SSRF 対策）。`https` かつ Spotify の CDN（`*.scdn.co`）のみ許可する。
/// URL は Spotify API 応答由来だが、多層防御として取得前に検証する純粋関数。
pub fn is_allowed_art_url(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            u.scheme() == "https"
                && u.host_str()
                    .is_some_and(|h| h == "scdn.co" || h.ends_with(".scdn.co"))
        }
        Err(_) => false,
    }
}

/// アート候補（`url`, `width`, `height`）から表示に使う URL を選ぶ純粋関数。
/// `TARGET_WIDTH` に最も近い横幅のものを選ぶ。空なら `None`。幅不明（0）でも他が無ければ選ぶ。
pub fn pick_image_url(images: &[(String, u32, u32)]) -> Option<String> {
    images
        .iter()
        .min_by_key(|(_, w, _)| w.abs_diff(TARGET_WIDTH))
        .map(|(url, _, _)| url.clone())
}

/// 画像を取得してデコードする。`reqwest` で本文を取り、`spawn_blocking` でデコードして
/// `DynamicImage` を返す（呼び出し側が `Picker::new_resize_protocol` に渡す）。
pub async fn fetch_decode(client: &reqwest::Client, url: &str) -> Result<DynamicImage> {
    // SSRF 対策: Spotify CDN 以外は取得しない（リダイレクトは呼び出し側で無効化済み）。
    if !is_allowed_art_url(url) {
        bail!("許可されていないカバーアート URL です");
    }
    let bytes = client
        .get(url)
        .send()
        .await
        .context("カバーアートの取得に失敗しました")?
        .error_for_status()
        .context("カバーアートの取得に失敗しました")?
        .bytes()
        .await
        .context("カバーアート本文の読み取りに失敗しました")?;
    // デコードは CPU バウンド。寸法上限（展開爆弾対策）を課して spawn_blocking で行う。
    let img = tokio::task::spawn_blocking(move || -> Result<DynamicImage> {
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .context("画像フォーマットの判定に失敗しました")?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_DIM);
        limits.max_image_height = Some(MAX_IMAGE_DIM);
        reader.limits(limits);
        reader.decode().context("画像のデコードに失敗しました")
    })
    .await
    .context("画像デコードタスクの実行に失敗しました")??;
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(url: &str, w: u32) -> (String, u32, u32) {
        (url.to_string(), w, w)
    }

    #[test]
    fn picks_width_closest_to_target() {
        // 640 / 300 / 64 のうち 300 が TARGET(300) に最も近い
        let imgs = [img("a640", 640), img("b300", 300), img("c64", 64)];
        assert_eq!(pick_image_url(&imgs).as_deref(), Some("b300"));
    }

    #[test]
    fn picks_nearest_when_no_exact() {
        // 200 と 500 では 200 の方が 300 に近い
        let imgs = [img("big", 500), img("small", 200)];
        assert_eq!(pick_image_url(&imgs).as_deref(), Some("small"));
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(pick_image_url(&[]), None);
    }

    #[test]
    fn unknown_width_still_selectable() {
        // 幅不明(0)でも唯一なら選ぶ
        let imgs = [img("only", 0)];
        assert_eq!(pick_image_url(&imgs).as_deref(), Some("only"));
    }

    #[test]
    fn allows_only_https_spotify_cdn() {
        assert!(is_allowed_art_url("https://i.scdn.co/image/ab67616d"));
        assert!(is_allowed_art_url("https://scdn.co/x"));
    }

    #[test]
    fn rejects_non_cdn_and_non_https() {
        // http は不可
        assert!(!is_allowed_art_url("http://i.scdn.co/image/x"));
        // 別ホスト（SSRF 対象）は不可
        assert!(!is_allowed_art_url(
            "https://169.254.169.254/latest/meta-data"
        ));
        assert!(!is_allowed_art_url("https://evil.com/scdn.co"));
        // scdn.co を含むが別ドメイン（サフィックス偽装）は不可
        assert!(!is_allowed_art_url("https://scdn.co.evil.com/x"));
        // 不正 URL
        assert!(!is_allowed_art_url("not a url"));
    }
}
