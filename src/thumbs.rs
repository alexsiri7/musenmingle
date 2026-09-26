//! Self-hosted thumbnails.
//!
//! We never hotlink venues' images (that would use their bandwidth on every
//! page view). Instead the ingest runner, after the sources of a tick, makes
//! ONE small thumbnail per source image: it fetches the image through the
//! [`FetchContext`] (robots.txt, User-Agent and per-domain rate limit apply),
//! downsizes it to fit [`ThumbConfig::max_width`] × [`ThumbConfig::max_height`]
//! and re-encodes it as JPEG, then stores it in `events.thumbnails`. The API
//! serves it at [`thumb_path`] with long cache headers, and pages credit the
//! source ("Image: Barbican") with a link to the event's page there.
//!
//! An image is fetched again only when the event's `image_url` changes (or,
//! after a failure, once [`ThumbConfig::retry_failed_after`] has passed).
//! Work per run and per image host is capped. Failures (robots.txt
//! disallows, oversized or undecodable images) are logged and recorded on the
//! thumbnail row; they are NOT source errors and never feed the health
//! checker.

use std::collections::HashMap;
use std::io::Cursor;
use std::time::Duration;

use chrono::{DateTime, Utc};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, Limits, RgbImage};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::fetch::FetchContext;
use crate::repo::{self, NewThumbnail, ThumbnailJob};

/// Content type of every stored thumbnail.
pub const THUMB_CONTENT_TYPE: &str = "image/jpeg";

/// Thumbnailer limits.
#[derive(Debug, Clone)]
pub struct ThumbConfig {
    pub max_width: u32,
    pub max_height: u32,
    /// First JPEG quality tried; lowered in steps until the output is under
    /// `target_bytes` (or `min_quality` is reached).
    pub quality: u8,
    pub min_quality: u8,
    pub target_bytes: usize,
    /// Source images larger than this are skipped.
    pub max_source_bytes: usize,
    /// Source images with more pixels on a side than this are skipped
    /// (decompression-bomb guard).
    pub max_source_dimension: u32,
    /// Thumbnails attempted per ingest run.
    pub per_run: usize,
    /// Thumbnails attempted per image host per ingest run.
    pub per_host: usize,
    /// A failed image URL is retried after this long.
    pub retry_failed_after: chrono::Duration,
    /// Wall-clock budget for the whole thumbnail pass.
    pub budget: Duration,
}

impl Default for ThumbConfig {
    fn default() -> Self {
        Self {
            max_width: 480,
            max_height: 480,
            quality: 70,
            min_quality: 45,
            target_bytes: 40 * 1024,
            max_source_bytes: 8 * 1024 * 1024,
            max_source_dimension: 12_000,
            per_run: 60,
            per_host: 20,
            retry_failed_after: chrono::Duration::days(7),
            budget: Duration::from_secs(300),
        }
    }
}

/// Public path of a thumbnail. The hash makes the URL change whenever the
/// bytes do, so it can be cached as immutable.
pub fn thumb_path(event_id: Uuid, content_hash: &str) -> String {
    format!("/thumbs/{event_id}-{content_hash}.jpg")
}

/// Parse the last segment of a thumbnail URL: `{uuid}-{hash}.jpg` or just
/// `{uuid}`. Returns the event id and the hash, if any.
pub fn parse_thumb_name(name: &str) -> Option<(Uuid, Option<&str>)> {
    if let Ok(id) = Uuid::parse_str(name) {
        return Some((id, None));
    }
    let stem = name.strip_suffix(".jpg")?;
    // A hyphenated UUID is 36 characters.
    let (id, rest) = stem.split_at_checked(36)?;
    let hash = rest.strip_prefix('-')?;
    if hash.is_empty() || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((Uuid::parse_str(id).ok()?, Some(hash)))
}

/// Why an image could not be turned into a thumbnail.
#[derive(Debug, thiserror::Error)]
pub enum ThumbError {
    #[error("could not decode image: {0}")]
    Decode(String),
    #[error("could not encode thumbnail: {0}")]
    Encode(String),
}

/// A thumbnail made by [`make_thumbnail`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumb {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decode `input` (JPEG, PNG, WebP or GIF), shrink it to fit the configured
/// box (never enlarging), flatten transparency onto white and encode it as
/// JPEG. Pure and CPU-bound: call it from `spawn_blocking`.
pub fn make_thumbnail(input: &[u8], cfg: &ThumbConfig) -> Result<Thumb, ThumbError> {
    let mut reader = ImageReader::new(Cursor::new(input))
        .with_guessed_format()
        .map_err(|e| ThumbError::Decode(e.to_string()))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(cfg.max_source_dimension);
    limits.max_image_height = Some(cfg.max_source_dimension);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| ThumbError::Decode(e.to_string()))?;
    let img = if img.width() > cfg.max_width || img.height() > cfg.max_height {
        img.resize(cfg.max_width, cfg.max_height, FilterType::Triangle)
    } else {
        img
    };
    let rgb = flatten_on_white(&img);
    let mut quality = cfg.quality.clamp(1, 100);
    loop {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&rgb)
            .map_err(|e| ThumbError::Encode(e.to_string()))?;
        if out.len() <= cfg.target_bytes || quality <= cfg.min_quality {
            return Ok(Thumb {
                bytes: out,
                width: rgb.width(),
                height: rgb.height(),
            });
        }
        quality = quality.saturating_sub(10).max(cfg.min_quality);
    }
}

fn flatten_on_white(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = u16::from(p[3]);
        let blend = |c: u8| ((u16::from(c) * a + 255 * (255 - a)) / 255) as u8;
        image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])])
    })
}

/// Short content hash used in thumbnail URLs and as the ETag.
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Outcome of one thumbnail pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThumbReport {
    pub made: usize,
    pub failed: usize,
    /// Jobs left for a later run (per-run / per-host caps, time budget).
    pub deferred: usize,
}

/// Make thumbnails for events that need one, within the configured caps.
pub async fn run(
    pool: &PgPool,
    ctx: &FetchContext,
    cfg: &ThumbConfig,
    now: DateTime<Utc>,
) -> sqlx::Result<ThumbReport> {
    let started = tokio::time::Instant::now();
    // Fetch more candidates than the run cap so busy hosts don't starve
    // the others.
    let limit = i64::try_from(cfg.per_run.saturating_mul(4)).unwrap_or(i64::MAX);
    let jobs = repo::thumbnail_jobs(pool, now, now - cfg.retry_failed_after, limit).await?;
    let mut report = ThumbReport::default();
    let mut per_host: HashMap<String, usize> = HashMap::new();
    for job in &jobs {
        let host = url::Url::parse(&job.image_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        let host_count = per_host.entry(host).or_default();
        if report.made + report.failed >= cfg.per_run
            || *host_count >= cfg.per_host
            || started.elapsed() >= cfg.budget
        {
            report.deferred += 1;
            continue;
        }
        *host_count += 1;
        match thumbnail_for(ctx, cfg, job).await {
            Ok(t) => {
                repo::save_thumbnail(pool, job, Ok(&t)).await?;
                report.made += 1;
            }
            Err(message) => {
                tracing::info!(event_id = %job.event_id, error = %message, "thumbnail skipped");
                repo::save_thumbnail(pool, job, Err(&message)).await?;
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

async fn thumbnail_for(
    ctx: &FetchContext,
    cfg: &ThumbConfig,
    job: &ThumbnailJob,
) -> Result<NewThumbnail, String> {
    let url = url::Url::parse(&job.image_url).map_err(|e| format!("invalid image URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("image URL is not http(s)".into());
    }
    let fetched = ctx
        .get_bytes_limited(&url, cfg.max_source_bytes)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(ct) = &fetched.content_type {
        let ct = ct.to_ascii_lowercase();
        if !ct.starts_with("image/") && !ct.starts_with("application/octet-stream") {
            return Err(format!("not an image (Content-Type {ct})"));
        }
    }
    let cfg2 = cfg.clone();
    let bytes = fetched.bytes;
    let thumb = tokio::task::spawn_blocking(move || make_thumbnail(&bytes, &cfg2))
        .await
        .map_err(|e| format!("thumbnail task failed: {e}"))?
        .map_err(|e| e.to_string())?;
    Ok(NewThumbnail {
        content_hash: content_hash(&thumb.bytes),
        content_type: THUMB_CONTENT_TYPE.into(),
        width: i32::try_from(thumb.width).unwrap_or(i32::MAX),
        height: i32::try_from(thumb.height).unwrap_or(i32::MAX),
        bytes: thumb.bytes,
        etag: fetched.etag,
        last_modified: fetched.last_modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};

    fn encode(img: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    /// A noisy photo-like image so JPEG sizes are realistic.
    fn photo(w: u32, h: u32) -> DynamicImage {
        let mut seed: u32 = 12345;
        DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let n = (seed >> 16) as u8 / 8;
            image::Rgb([
                ((x * 255 / w) as u8).saturating_add(n),
                ((y * 255 / h) as u8).saturating_add(n),
                (((x + y) * 128 / (w + h)) as u8).saturating_add(n),
            ])
        }))
    }

    #[test]
    fn large_image_is_shrunk_into_the_box_and_small_file() {
        let cfg = ThumbConfig::default();
        for (w, h, want) in [(1600, 900, (480, 270)), (900, 1600, (270, 480))] {
            let t = make_thumbnail(&encode(photo(w, h), ImageFormat::Png), &cfg).unwrap();
            assert_eq!((t.width, t.height), want);
            assert!(t.bytes.len() <= cfg.target_bytes, "{} bytes", t.bytes.len());
            let back = image::load_from_memory_with_format(&t.bytes, ImageFormat::Jpeg).unwrap();
            assert_eq!((back.width(), back.height()), want);
        }
    }

    #[test]
    fn small_image_is_not_enlarged_and_alpha_is_flattened() {
        let transparent = DynamicImage::ImageRgba8(RgbaImage::from_pixel(40, 30, Rgba([0; 4])));
        let t = make_thumbnail(
            &encode(transparent, ImageFormat::Png),
            &ThumbConfig::default(),
        )
        .unwrap();
        assert_eq!((t.width, t.height), (40, 30));
        let back = image::load_from_memory(&t.bytes).unwrap().to_rgb8();
        assert!(back.pixels().all(|p| p.0.iter().all(|c| *c > 240)), "white");
    }

    #[test]
    fn webp_and_gif_inputs_decode() {
        let cfg = ThumbConfig::default();
        for f in [ImageFormat::WebP, ImageFormat::Gif, ImageFormat::Jpeg] {
            let img = DynamicImage::ImageRgba8(photo(600, 400).to_rgba8());
            let img = if f == ImageFormat::Jpeg {
                DynamicImage::ImageRgb8(img.to_rgb8())
            } else {
                img
            };
            let t = make_thumbnail(&encode(img, f), &cfg).unwrap();
            assert_eq!((t.width, t.height), (480, 320), "{f:?}");
        }
    }

    #[test]
    fn garbage_and_oversized_dimensions_are_rejected() {
        let cfg = ThumbConfig::default();
        assert!(make_thumbnail(b"<html>not an image</html>", &cfg).is_err());
        let small_limit = ThumbConfig {
            max_source_dimension: 100,
            ..ThumbConfig::default()
        };
        let err = make_thumbnail(&encode(photo(200, 50), ImageFormat::Png), &small_limit);
        assert!(err.is_err());
    }

    #[test]
    fn thumb_names_round_trip() {
        let id = Uuid::new_v4();
        let path = thumb_path(id, "0123abcd");
        let name = path.strip_prefix("/thumbs/").unwrap();
        assert_eq!(parse_thumb_name(name), Some((id, Some("0123abcd"))));
        assert_eq!(parse_thumb_name(&id.to_string()), Some((id, None)));
        for bad in [
            "x",
            "",
            &format!("{id}-.jpg"),
            &format!("{id}-zz.jpg"),
            &format!("{id}-00.png"),
            &format!("{id}00.jpg"),
        ] {
            assert_eq!(parse_thumb_name(bad), None, "{bad}");
        }
        assert_eq!(content_hash(b"abc").len(), 16);
    }
}
