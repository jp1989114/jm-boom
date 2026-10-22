use super::{cache_index_key, storage::temporary_path, CacheConfig, ImageCache};
use crate::reader::PageImageFormat;
use sqlx::sqlite::SqlitePoolOptions;
use std::{
    io::Cursor,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::{fs, task::JoinSet};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[tokio::test]
async fn tracks_replacement_size_delta_and_resets_after_clear() {
    let (cache, cache_dir) = test_cache("replace", 100).await;

    cache
        .put_cover("cover", &[1; 20])
        .await
        .expect("write initial cache entry");
    assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), 20);

    cache
        .put_cover("cover", &[2; 8])
        .await
        .expect("replace cache entry");
    assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), 8);
    assert_eq!(
        cache.get_cover("cover").await.expect("read replacement"),
        Some(vec![2; 8])
    );
    let stats = cache.stats().await.expect("load cache stats");
    assert_eq!(stats.size_bytes, 8);
    assert_eq!(stats.entry_count, 1);

    cache.clear().await.expect("clear cache");
    assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), 0);
    fs::remove_dir_all(cache_dir)
        .await
        .expect("remove test cache");
}

#[tokio::test]
async fn evicts_to_half_capacity_only_after_limit_is_exceeded() {
    let (cache, cache_dir) = test_cache("evict", 10).await;

    cache.put_cover("one", &[1; 4]).await.expect("write one");
    cache.put_cover("two", &[2; 4]).await.expect("write two");
    let before = cache.stats().await.expect("load pre-eviction stats");
    assert_eq!(before.size_bytes, 8);
    assert_eq!(before.entry_count, 2);

    cache
        .put_cover("three", &[3; 4])
        .await
        .expect("write over capacity");
    let after = cache.stats().await.expect("load post-eviction stats");
    assert_eq!(after.size_bytes, 4);
    assert_eq!(after.entry_count, 1);
    assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), 4);

    fs::remove_dir_all(cache_dir)
        .await
        .expect("remove test cache");
}

#[tokio::test]
async fn calibrates_size_from_repaired_index_on_startup() {
    let db = test_db().await;
    let cache_dir = test_cache_dir("calibrate");
    fs::create_dir_all(&cache_dir)
        .await
        .expect("create test cache directory");
    let path = cache_dir.join("seed.img");
    fs::write(&path, [1; 7])
        .await
        .expect("write seeded cache file");
    sqlx::query(
        "INSERT INTO cache_index (key, path, size, created_at, accessed_at) VALUES (?, ?, ?, 1, 1)",
    )
    .bind("seed:img")
    .bind(path.to_string_lossy().as_ref())
    .bind(99_i64)
    .execute(&db)
    .await
    .expect("seed cache index");

    let cache = ImageCache::new_with_cache_dir(
        db,
        CacheConfig {
            max_size_bytes: 100,
        },
        cache_dir.clone(),
    )
    .await
    .expect("create calibrated cache");

    assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), 7);
    assert_eq!(cache.stats().await.expect("load cache stats").size_bytes, 7);
    fs::remove_dir_all(cache_dir)
        .await
        .expect("remove test cache");
}

#[tokio::test]
async fn ignores_and_repairs_an_uncommitted_cache_file() {
    let db = test_db().await;
    let cache_dir = test_cache_dir("uncommitted");
    let cache = ImageCache::new_with_cache_dir(
        db.clone(),
        CacheConfig {
            max_size_bytes: 100,
        },
        cache_dir.clone(),
    )
    .await
    .expect("create initial cache");
    let final_path = cache.storage.path_for("cover", "img");
    let temporary_path = temporary_path(&final_path).expect("create temporary cache path");
    fs::write(&temporary_path, [1; 7])
        .await
        .expect("write interrupted cache file");

    assert!(cache
        .get_cover("cover")
        .await
        .expect("read cache before repair")
        .is_none());
    drop(cache);

    let repaired = ImageCache::new_with_cache_dir(
        db,
        CacheConfig {
            max_size_bytes: 100,
        },
        cache_dir.clone(),
    )
    .await
    .expect("repair cache");
    assert!(!temporary_path.exists());
    assert!(repaired
        .get_cover("cover")
        .await
        .expect("read repaired cache")
        .is_none());

    drop(repaired);
    fs::remove_dir_all(cache_dir)
        .await
        .expect("remove test cache");
}

#[tokio::test]
async fn roundtrips_complete_reader_images_without_changing_bytes() {
    let (cache, cache_dir) = test_cache("reader-formats", 10_000).await;
    let mut total_size = 0;
    for (index, format) in PageImageFormat::supported().into_iter().enumerate() {
        let key = format!("reader:{index}");
        let data = page_image(format);
        assert!(format.is_complete(&data));
        cache.put_reader_page(&key, format, &data).await.unwrap();
        let cached = cache.get_reader_page(&key).await.unwrap().unwrap();
        assert_eq!(cached.format, format);
        assert_eq!(cached.data, data);
        total_size += data.len() as i64;
    }
    let stats = cache.stats().await.unwrap();
    assert_eq!(stats.entry_count, 4);
    assert_eq!(stats.size_bytes, total_size);
    assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), total_size);
    fs::remove_dir_all(cache_dir).await.unwrap();
}

#[tokio::test]
async fn rejects_truncated_or_mismatched_reader_images_before_cache_writes() {
    let (cache, cache_dir) = test_cache("reader-invalid-write", 10_000).await;
    for format in PageImageFormat::supported() {
        let mut data = page_image(format);
        data.pop();
        assert_eq!(PageImageFormat::detect(&data).unwrap(), format);
        assert!(cache
            .put_reader_page("reader:0", format, &data)
            .await
            .is_err());
        assert!(!cache
            .storage
            .path_for("reader:0", format.extension())
            .exists());
    }
    assert!(cache
        .put_reader_page(
            "reader:0",
            PageImageFormat::WebP,
            &page_image(PageImageFormat::Png)
        )
        .await
        .is_err());
    let stats = cache.stats().await.unwrap();
    assert_eq!(stats.entry_count, 0);
    assert_eq!(stats.size_bytes, 0);
    assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), 0);
    assert!(cache.storage.scan_files().await.unwrap().is_empty());
    fs::remove_dir_all(cache_dir).await.unwrap();
}

#[tokio::test]
async fn removes_truncated_reader_files_indexes_and_pending_accesses() {
    let (cache, cache_dir) = test_cache("reader-invalid-read", 10_000).await;
    cache.put_cover("unrelated-cover", &[7; 17]).await.unwrap();
    for (index, format) in PageImageFormat::supported().into_iter().enumerate() {
        let key = format!("legacy:{index}");
        let mut data = page_image(format);
        data.pop();
        // Seed a legacy entry through the raw storage path, bypassing new write validation.
        cache
            .put_with_extension(&key, format.extension(), &data)
            .await
            .unwrap();
        let index_key = cache_index_key(&key, format.extension());
        cache.record_access(&index_key).await;
        assert!(cache.get_reader_page(&key).await.unwrap().is_none());
        assert!(!cache.storage.path_for(&key, format.extension()).exists());
        assert_eq!(cache.repository.entry_size(&index_key).await.unwrap(), 0);
        assert!(!cache.pending_accesses.lock().await.contains_key(&index_key));
        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.size_bytes, 17);
        assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), 17);
    }
    assert_eq!(cache.flush_accesses().await.unwrap(), 0);
    assert_eq!(
        cache.get_cover("unrelated-cover").await.unwrap(),
        Some(vec![7; 17])
    );
    fs::remove_dir_all(cache_dir).await.unwrap();
}

#[tokio::test]
async fn removes_a_mismatched_format_and_reads_a_valid_alternative() {
    let (cache, cache_dir) = test_cache("reader-format-mismatch", 10_000).await;
    let data = page_image(PageImageFormat::Png);
    cache
        .put_with_extension("reader:0", "webp", &data)
        .await
        .unwrap();
    cache
        .put_reader_page("reader:0", PageImageFormat::Png, &data)
        .await
        .unwrap();
    let page = cache.get_reader_page("reader:0").await.unwrap().unwrap();
    assert_eq!(page.format, PageImageFormat::Png);
    assert_eq!(page.data, data);
    assert!(!cache.storage.path_for("reader:0", "webp").exists());
    assert_eq!(cache.repository.entry_size("reader:0").await.unwrap(), 0);
    let stats = cache.stats().await.unwrap();
    assert_eq!(stats.entry_count, 1);
    assert_eq!(stats.size_bytes, data.len() as i64);
    assert_eq!(
        cache.current_size_bytes.load(Ordering::Relaxed),
        data.len() as i64
    );
    fs::remove_dir_all(cache_dir).await.unwrap();
}

#[tokio::test]
async fn an_invalid_replacement_preserves_the_existing_reader_image() {
    let (cache, cache_dir) = test_cache("reader-invalid-replacement", 10_000).await;
    let data = page_image(PageImageFormat::Png);
    cache
        .put_reader_page("reader:0", PageImageFormat::Png, &data)
        .await
        .unwrap();
    assert!(cache
        .put_reader_page("reader:0", PageImageFormat::Png, &data[..data.len() - 1])
        .await
        .is_err());
    let page = cache.get_reader_page("reader:0").await.unwrap().unwrap();
    assert_eq!(page.data, data);
    assert_eq!(cache.stats().await.unwrap().size_bytes, data.len() as i64);
    assert_eq!(
        cache.current_size_bytes.load(Ordering::Relaxed),
        data.len() as i64
    );
    fs::remove_dir_all(cache_dir).await.unwrap();
}

#[tokio::test]
async fn concurrent_invalid_reader_reads_remove_the_index_size_only_once() {
    let (cache, cache_dir) = test_cache("reader-concurrent-repair", 10_000).await;
    cache
        .put_cover("unrelated-cover", &[7; 1000])
        .await
        .unwrap();
    let mut data = page_image(PageImageFormat::Png);
    data.pop();
    cache
        .put_with_extension("reader:0", "png", &data)
        .await
        .unwrap();
    let cache = Arc::new(cache);
    let mut readers = JoinSet::new();
    for _ in 0..8 {
        let cache = cache.clone();
        readers.spawn(async move { cache.get_reader_page("reader:0").await });
    }
    while let Some(result) = readers.join_next().await {
        assert!(result.unwrap().unwrap().is_none());
    }
    let stats = cache.stats().await.unwrap();
    assert_eq!(stats.entry_count, 1);
    assert_eq!(stats.size_bytes, 1000);
    assert_eq!(cache.current_size_bytes.load(Ordering::Relaxed), 1000);
    fs::remove_dir_all(cache_dir).await.unwrap();
}

#[tokio::test]
async fn rechecking_a_stale_invalid_read_preserves_a_complete_replacement() {
    let (cache, cache_dir) = test_cache("reader-recheck-replacement", 10_000).await;
    let data = page_image(PageImageFormat::Png);
    cache
        .put_with_extension("reader:0", "png", &data[..data.len() - 1])
        .await
        .unwrap();
    let stale = cache
        .get_with_extension("reader:0", "png")
        .await
        .unwrap()
        .unwrap();
    assert!(!PageImageFormat::Png.is_complete(&stale));
    cache
        .put_reader_page("reader:0", PageImageFormat::Png, &data)
        .await
        .unwrap();
    let page = cache
        .recheck_reader_page("reader:0", PageImageFormat::Png)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.data, data);
    assert!(cache.storage.path_for("reader:0", "png").exists());
    assert_eq!(cache.stats().await.unwrap().entry_count, 1);
    assert_eq!(
        cache.current_size_bytes.load(Ordering::Relaxed),
        data.len() as i64
    );
    fs::remove_dir_all(cache_dir).await.unwrap();
}

fn page_image(format: PageImageFormat) -> Vec<u8> {
    let image =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3])));
    let format = match format {
        PageImageFormat::Gif => image::ImageFormat::Gif,
        PageImageFormat::Jpeg => image::ImageFormat::Jpeg,
        PageImageFormat::Png => image::ImageFormat::Png,
        PageImageFormat::WebP => image::ImageFormat::WebP,
    };
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, format)
        .expect("encode test image");
    bytes.into_inner()
}

async fn test_cache(name: &str, max_size_bytes: i64) -> (ImageCache, PathBuf) {
    let cache_dir = test_cache_dir(name);
    let cache = ImageCache::new_with_cache_dir(
        test_db().await,
        CacheConfig { max_size_bytes },
        cache_dir.clone(),
    )
    .await
    .expect("create test cache");
    (cache, cache_dir)
}

async fn test_db() -> sqlx::SqlitePool {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("run migrations");
    db
}

fn test_cache_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "jm-boom-image-cache-{name}-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}
