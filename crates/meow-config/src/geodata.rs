use crate::internal_http;
use crate::raw::{RawGeoDataConfig, RawGeoXUrl};
use anyhow::anyhow;
use meow_common::adapter::Proxy;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Default download URLs, aligned with Go mihomo v1.19.27.
const DEFAULT_MMDB_URL: &str =
    "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.metadb";
const DEFAULT_ASN_URL: &str =
    "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/GeoLite2-ASN.mmdb";
const DEFAULT_GEOSITE_URL: &str =
    "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geosite.dat";

/// Validated geodata config, produced by [`parse_geodata`].
#[derive(Debug, Clone)]
pub struct GeoDataConfig {
    pub mmdb_path: Option<PathBuf>,
    pub asn_path: Option<PathBuf>,
    pub geosite_path: Option<PathBuf>,
    pub auto_update: bool,
    /// Hours between update checks (≥1).
    pub auto_update_interval: u32,
    /// URL for binary GeoIP database (`geoip.dat`). Aligned with Go's
    /// `geox-url.geoip`. Not used by meow-rs (MMDB-only), but accepted for
    /// upstream-config compatibility.
    pub geo_ip_url: Option<String>,
    pub mmdb_url: String,
    pub asn_url: String,
    pub geosite_url: String,
}

impl Default for GeoDataConfig {
    fn default() -> Self {
        Self {
            mmdb_path: None,
            asn_path: None,
            geosite_path: None,
            auto_update: false,
            auto_update_interval: 24,
            geo_ip_url: None,
            mmdb_url: DEFAULT_MMDB_URL.to_string(),
            asn_url: DEFAULT_ASN_URL.to_string(),
            geosite_url: DEFAULT_GEOSITE_URL.to_string(),
        }
    }
}

/// Resolve a download URL: `geox-url:<key>` if set, else `default`.
fn resolve_url(geox_val: Option<&str>, default: &str) -> String {
    geox_val.unwrap_or(default).to_string()
}

/// Parse and validate geodata config from `geodata:` and `geox-url:` blocks.
///
/// Download URLs use the top-level `geox-url:` block if present,
/// falling back to built-in defaults aligned with Go v1.19.27.
///
/// Returns `GeoDataConfig::default()` when both blocks are absent.
pub fn parse_geodata(
    raw: Option<&RawGeoDataConfig>,
    geox_url: Option<&RawGeoXUrl>,
) -> Result<GeoDataConfig, anyhow::Error> {
    let default_geox = RawGeoXUrl::default();
    let gx = geox_url.unwrap_or(&default_geox);

    let Some(r) = raw else {
        return Ok(GeoDataConfig {
            geo_ip_url: gx.geo_ip.clone(),
            mmdb_url: gx.mmdb.clone().unwrap_or_else(|| DEFAULT_MMDB_URL.to_string()),
            asn_url: gx.asn.clone().unwrap_or_else(|| DEFAULT_ASN_URL.to_string()),
            geosite_url: gx.geosite.clone().unwrap_or_else(|| DEFAULT_GEOSITE_URL.to_string()),
            ..GeoDataConfig::default()
        });
    };

    // Warn on upstream-only fields (Class B per ADR-0002 §geodata-subsection.md).
    for (name, val) in [
        ("geodata-mode", &r.geodata_mode),
        ("geodata-loader", &r.geodata_loader),
        ("geoip-matcher", &r.geoip_matcher),
    ] {
        if val.is_some() {
            warn!(
                "geodata.{}: field is not supported in meow-rs and will be ignored \
                (upstream: config.go); remove it to suppress this warning",
                name
            );
        }
    }

    let interval = r.auto_update_interval.unwrap_or(24);
    if interval == 0 {
        return Err(anyhow!(
            "geodata.auto-update-interval must be at least 1 hour (got 0)"
        ));
    }

    Ok(GeoDataConfig {
        mmdb_path: r.mmdb_path.as_deref().map(PathBuf::from),
        asn_path: r.asn_path.as_deref().map(PathBuf::from),
        geosite_path: r.geosite_path.as_deref().map(PathBuf::from),
        auto_update: r.auto_update,
        auto_update_interval: interval,
        geo_ip_url: gx.geo_ip.clone(),
        mmdb_url: resolve_url(gx.mmdb.as_deref(), DEFAULT_MMDB_URL),
        asn_url: resolve_url(gx.asn.as_deref(), DEFAULT_ASN_URL),
        geosite_url: resolve_url(gx.geosite.as_deref(), DEFAULT_GEOSITE_URL),
    })
}

/// Download `url` and atomically replace `dest` via a `.tmp` sibling.
///
/// When `proxy` is `Some`, the HTTP fetch is tunneled through that proxy
/// adapter (used so GFW-blocked CDNs stay reachable on background refresh);
/// otherwise the OS handles connectivity directly.
///
/// Returns `Ok(())` on success. On failure the temp file is removed (best-
/// effort) and the original `dest` is untouched.
pub async fn download_and_replace(
    url: &str,
    dest: &Path,
    proxy: Option<&Arc<dyn Proxy>>,
) -> Result<(), anyhow::Error> {
    let tmp = dest.with_extension("tmp");

    if let Some(p) = proxy {
        info!(
            "auto-update: downloading {} from {} via proxy '{}'",
            dest.display(),
            url,
            p.name()
        );
    } else {
        info!("auto-update: downloading {} from {}", dest.display(), url);
    }

    let bytes = if let Some(p) = proxy {
        internal_http::fetch_via_proxy(url, p).await?
    } else {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent(concat!("clash.meta/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let resp = client.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("HTTP {status} fetching {url}"));
        }
        resp.bytes().await?.to_vec()
    };

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&tmp, &bytes)?;

    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!(
            "atomic rename {} → {}: {}",
            tmp.display(),
            dest.display(),
            e
        ));
    }

    info!(
        "auto-update: {} updated ({} bytes)",
        dest.display(),
        bytes.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::{RawGeoDataConfig, RawGeoXUrl};

    fn raw_defaults() -> RawGeoDataConfig {
        RawGeoDataConfig::default()
    }

    #[test]
    fn absent_block_returns_defaults() {
        let cfg = parse_geodata(None, None).unwrap();
        assert!(!cfg.auto_update);
        assert_eq!(cfg.auto_update_interval, 24);
        assert!(cfg.mmdb_path.is_none());
        assert!(cfg.asn_path.is_none());
        assert!(cfg.geosite_path.is_none());
        assert!(cfg.geo_ip_url.is_none());
        assert!(cfg.mmdb_url.contains("geoip.metadb"));
        assert!(cfg.asn_url.contains("GeoLite2-ASN"));
        assert!(cfg.geosite_url.contains("geosite.dat"));
    }

    #[test]
    fn explicit_paths_override_discovery() {
        let raw = RawGeoDataConfig {
            mmdb_path: Some("/custom/Country.mmdb".to_string()),
            asn_path: Some("/custom/ASN.mmdb".to_string()),
            geosite_path: Some("/custom/geosite.mrs".to_string()),
            ..raw_defaults()
        };
        let cfg = parse_geodata(Some(&raw), None).unwrap();
        assert_eq!(
            cfg.mmdb_path.unwrap().to_str().unwrap(),
            "/custom/Country.mmdb"
        );
        assert_eq!(cfg.asn_path.unwrap().to_str().unwrap(), "/custom/ASN.mmdb");
        assert_eq!(
            cfg.geosite_path.unwrap().to_str().unwrap(),
            "/custom/geosite.mrs"
        );
    }

    #[test]
    fn geox_url_overrides_defaults() {
        let gx = RawGeoXUrl {
            mmdb: Some("https://example.com/geoip.metadb".to_string()),
            geosite: Some("https://example.com/geosite.mrs".to_string()),
            ..Default::default()
        };
        let cfg = parse_geodata(None, Some(&gx)).unwrap();
        assert_eq!(cfg.mmdb_url, "https://example.com/geoip.metadb");
        assert!(cfg.asn_url.contains("GeoLite2-ASN")); // default preserved
        assert_eq!(cfg.geosite_url, "https://example.com/geosite.mrs");
    }

    #[test]
    fn interval_zero_is_hard_error() {
        let raw = RawGeoDataConfig {
            auto_update_interval: Some(0),
            ..raw_defaults()
        };
        let err = parse_geodata(Some(&raw), None).unwrap_err();
        assert!(
            err.to_string().contains("at least 1 hour"),
            "error should mention minimum interval: {err}"
        );
    }

    #[test]
    fn absent_interval_defaults_to_24() {
        let raw = RawGeoDataConfig {
            auto_update: true,
            auto_update_interval: None,
            ..raw_defaults()
        };
        let cfg = parse_geodata(Some(&raw), None).unwrap();
        assert_eq!(cfg.auto_update_interval, 24);
    }

    #[test]
    fn upstream_only_fields_do_not_error() {
        // geodata-mode, geodata-loader, geoip-matcher accepted without error.
        let raw = RawGeoDataConfig {
            geodata_mode: Some(serde_yaml::Value::String("memconservative".to_string())),
            geodata_loader: Some(serde_yaml::Value::String("standard".to_string())),
            geoip_matcher: Some(serde_yaml::Value::String("succinct".to_string())),
            ..raw_defaults()
        };
        // Must not error — warn-only (Class B per ADR-0002).
        parse_geodata(Some(&raw), None).unwrap();
    }

    #[test]
    fn geox_url_alone_provides_defaults() {
        // When geodata block is absent but geox-url is present.
        let gx = RawGeoXUrl {
            mmdb: Some("https://custom/mmdb".to_string()),
            ..Default::default()
        };
        let cfg = parse_geodata(None, Some(&gx)).unwrap();
        assert_eq!(cfg.mmdb_url, "https://custom/mmdb");
        assert!(cfg.asn_url.contains("GeoLite2-ASN")); // default
        assert!(cfg.geosite_url.contains("geosite.dat")); // default
        assert!(cfg.geo_ip_url.is_none());
    }

    #[test]
    fn geox_url_geoip_field_stored() {
        let gx = RawGeoXUrl {
            geo_ip: Some("https://example/geoip.dat".to_string()),
            ..Default::default()
        };
        let cfg = parse_geodata(None, Some(&gx)).unwrap();
        assert_eq!(cfg.geo_ip_url.as_deref(), Some("https://example/geoip.dat"));
    }
}
