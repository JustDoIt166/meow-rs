use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use flate2::read::GzDecoder;
use meow_config::ExternalUiConfig;
use percent_encoding::percent_decode_str;
use std::{
    io::Cursor,
    path::{Component, Path as StdPath, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::routes::AppState;

const UI_HTML: &str = include_str!("../static/index.html");

#[derive(Clone)]
pub enum UiAssets {
    BuiltIn,
    External(ExternalUiAssets),
}

impl UiAssets {
    pub fn built_in() -> Self {
        Self::BuiltIn
    }

    pub fn from_config(config: &meow_config::UiConfig) -> Self {
        match config.external.as_ref() {
            Some(external) => Self::External(ExternalUiAssets::new(external.clone())),
            None => Self::BuiltIn,
        }
    }

    pub async fn auto_download_if_empty(&self) {
        let Self::External(external) = self else {
            return;
        };

        if !dir_is_empty_or_missing(&external.path).await {
            info!(
                "external UI already exists at {}, skipping download",
                external.path.display()
            );
            return;
        }

        info!(
            "external UI missing or empty, downloading from {}",
            external.url
        );
        if let Err(e) = external.download_and_replace().await {
            warn!("external UI download failed: {e:#}");
        }
    }

    pub async fn upgrade(&self) -> Result<(), anyhow::Error> {
        match self {
            Self::BuiltIn => anyhow::bail!("external UI is not configured"),
            Self::External(external) => external.download_and_replace().await,
        }
    }
}

#[derive(Clone)]
pub struct ExternalUiAssets {
    path: PathBuf,
    url: String,
    update_lock: Arc<Mutex<()>>,
}

impl ExternalUiAssets {
    fn new(config: ExternalUiConfig) -> Self {
        Self {
            path: config.path,
            url: config.url,
            update_lock: Arc::new(Mutex::new(())),
        }
    }

    async fn download_and_replace(&self) -> Result<(), anyhow::Error> {
        let _guard = self.update_lock.lock().await;
        let bytes = reqwest::get(&self.url)
            .await?
            .error_for_status()?
            .bytes()
            .await?;

        let temp = tempfile::Builder::new().prefix("meow-ui-").tempdir()?;
        extract_archive(&bytes, temp.path())?;
        replace_dir_contents(temp.path(), &self.path)?;
        Ok(())
    }
}

pub async fn serve_ui_root(State(state): State<Arc<AppState>>) -> Response {
    match &state.ui_assets {
        UiAssets::BuiltIn => Html(UI_HTML).into_response(),
        UiAssets::External(_) => Redirect::temporary("/ui/").into_response(),
    }
}

pub async fn serve_ui_index(State(state): State<Arc<AppState>>) -> Response {
    match &state.ui_assets {
        UiAssets::BuiltIn => Html(UI_HTML).into_response(),
        UiAssets::External(external) => serve_external(external, "").await,
    }
}

pub async fn serve_ui_path(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
) -> Response {
    match &state.ui_assets {
        UiAssets::BuiltIn => Html(UI_HTML).into_response(),
        UiAssets::External(external) => serve_external(external, &rest).await,
    }
}

async fn serve_external(external: &ExternalUiAssets, rest: &str) -> Response {
    let decoded = match percent_decode_str(rest).decode_utf8() {
        Ok(s) => s,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let rel = match clean_request_path(&decoded) {
        Ok(rel) => rel,
        Err(_) => return StatusCode::FORBIDDEN.into_response(),
    };

    let requested = if rel.as_os_str().is_empty() {
        external.path.join("index.html")
    } else {
        external.path.join(&rel)
    };

    match serve_file_inside(&external.path, &requested).await {
        Ok(resp) => resp,
        Err(FileServeError::NotFound) if rel.extension().is_none() => {
            serve_file_inside(&external.path, &external.path.join("index.html"))
                .await
                .unwrap_or_else(|e| e.into_response())
        }
        Err(e) => e.into_response(),
    }
}

fn clean_request_path(rest: &str) -> Result<PathBuf, ()> {
    let mut out = PathBuf::new();
    if rest.is_empty() {
        return Ok(out);
    }

    for component in StdPath::new(rest).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return Err(()),
        }
    }
    Ok(out)
}

async fn serve_file_inside(root: &StdPath, file: &StdPath) -> Result<Response, FileServeError> {
    let root = tokio::fs::canonicalize(root)
        .await
        .map_err(|_| FileServeError::NotFound)?;
    let meta = tokio::fs::metadata(file)
        .await
        .map_err(|_| FileServeError::NotFound)?;
    if meta.is_dir() {
        return Err(FileServeError::NotFound);
    }
    let file = tokio::fs::canonicalize(file)
        .await
        .map_err(|_| FileServeError::NotFound)?;
    if !file.starts_with(&root) {
        return Err(FileServeError::Forbidden);
    }

    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|e| FileServeError::Io(e.to_string()))?;
    let content_type = content_type_for(&file);
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        Body::from(bytes),
    )
        .into_response())
}

fn content_type_for(path: &StdPath) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

enum FileServeError {
    NotFound,
    Forbidden,
    Io(String),
}

impl FileServeError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND.into_response(),
            Self::Forbidden => StatusCode::FORBIDDEN.into_response(),
            Self::Io(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }
}

async fn dir_is_empty_or_missing(path: &StdPath) -> bool {
    let Ok(mut entries) = tokio::fs::read_dir(path).await else {
        return true;
    };
    entries.next_entry().await.ok().flatten().is_none()
}

fn extract_archive(bytes: &[u8], dest: &StdPath) -> Result<(), anyhow::Error> {
    if bytes.starts_with(b"PK\x03\x04") {
        extract_zip(bytes, dest)
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        extract_tar_gz(bytes, dest)
    } else {
        anyhow::bail!("unknown or unsupported UI archive type");
    }
}

fn extract_zip(bytes: &[u8], dest: &StdPath) -> Result<(), anyhow::Error> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        if file.is_symlink() {
            continue;
        }
        let Some(enclosed) = file.enclosed_name() else {
            anyhow::bail!("UI archive contains unsafe path: {}", file.name());
        };
        let out = dest.join(enclosed);
        ensure_local_child(dest, &out)?;
        if file.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out_file = std::fs::File::create(&out)?;
        std::io::copy(&mut file, &mut out_file)?;
    }
    Ok(())
}

fn extract_tar_gz(bytes: &[u8], dest: &StdPath) -> Result<(), anyhow::Error> {
    let gz = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            continue;
        }
        if !kind.is_dir() && !kind.is_file() {
            continue;
        }
        let rel = entry.path()?;
        let out = dest.join(rel.as_ref());
        ensure_local_child(dest, &out)?;
        if kind.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out_file = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut out_file)?;
    }
    Ok(())
}

fn ensure_local_child(root: &StdPath, child: &StdPath) -> Result<(), anyhow::Error> {
    let rel = child.strip_prefix(root)?;
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("UI archive contains path traversal: {}", child.display());
            }
        }
    }
    Ok(())
}

fn replace_dir_contents(src: &StdPath, dst: &StdPath) -> Result<(), anyhow::Error> {
    let src = collapse_single_root(src)?;
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(dst)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    }

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        match std::fs::rename(&src_path, &dst_path) {
            Ok(()) => {}
            Err(_) => copy_recursively(&src_path, &dst_path)?,
        }
    }
    Ok(())
}

fn collapse_single_root(src: &StdPath) -> Result<PathBuf, anyhow::Error> {
    let entries = std::fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;
    if entries.len() == 1 && entries[0].file_type()?.is_dir() {
        Ok(entries[0].path())
    } else {
        Ok(src.to_path_buf())
    }
}

fn copy_recursively(src: &StdPath, dst: &StdPath) -> Result<(), anyhow::Error> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        anyhow::bail!(
            "refusing to copy symlink from UI archive: {}",
            src.display()
        );
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursively(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn zip_single_root_extracts_to_target_root() {
        let mut zip_data = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut zip_data);
            let opts = zip::write::SimpleFileOptions::default();
            zip.add_directory("root/", opts).unwrap();
            zip.start_file("root/index.html", opts).unwrap();
            zip.write_all(b"ok").unwrap();
            zip.finish().unwrap();
        }

        let extracted = tempfile::tempdir().unwrap();
        extract_archive(zip_data.get_ref(), extracted.path()).unwrap();
        let target = tempfile::tempdir().unwrap();
        replace_dir_contents(extracted.path(), target.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.path().join("index.html")).unwrap(),
            "ok"
        );
    }

    #[test]
    fn zip_traversal_is_rejected() {
        let mut zip_data = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut zip_data);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("../evil.txt", opts).unwrap();
            zip.write_all(b"bad").unwrap();
            zip.finish().unwrap();
        }

        let extracted = tempfile::tempdir().unwrap();
        assert!(extract_archive(zip_data.get_ref(), extracted.path()).is_err());
    }

    #[test]
    fn tar_gz_extracts_to_target_root() {
        let mut tar_bytes = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::default());
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_path("root/index.html").unwrap();
            header.set_size(2);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, Cursor::new(b"ok")).unwrap();
            tar.finish().unwrap();
        }

        let extracted = tempfile::tempdir().unwrap();
        extract_archive(&tar_bytes, extracted.path()).unwrap();
        let target = tempfile::tempdir().unwrap();
        replace_dir_contents(extracted.path(), target.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.path().join("index.html")).unwrap(),
            "ok"
        );
    }
}
