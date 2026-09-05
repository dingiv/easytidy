//! shared — unified asset + data file resolution for cargo crates.
//!
//! ## Namespaces from Cargo.toml (zero-config in calling code)
//!
//! Declare namespaces in the crate's `Cargo.toml`:
//! ```toml
//! [package.metadata.shared]
//! CONF_DIR  = { dev = "data/conf",  prod = "~/.my-app/conf" }
//! MODEL_DIR = { dev = "data/model", prod = "~/.my-app/model" }
//! ```
//!
//! Add a one-line `build.rs`:
//! ```rust,ignore
//! fn main() { shared::emit_namespaces(); }
//! ```
//!
//! In code, `loader!()` auto-discovers all declared namespaces — the caller never sees paths:
//! ```ignore
//! let fs = loader!();                              // auto-loaded from Cargo.toml
//! let cfg  = fs.read_str("CONF_DIR::my.conf")?;    // dev→data/conf/  prod→~/.my-app/conf/
//! let logo = fs.read("logo.png")?;                 // bare = asset: dev→assets/  prod→exe/assets/
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

// ── Namespace ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Namespace {
    pub dev: String,
    pub prod: String,
}

// ── FileLoader ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileLoader {
    manifest_dir: PathBuf,
    assets_subdir: String,
    namespaces: HashMap<String, Namespace>,
}

impl FileLoader {
    pub fn new(manifest_dir: impl Into<PathBuf>, assets_subdir: impl Into<String>) -> Self {
        Self {
            manifest_dir: manifest_dir.into(),
            assets_subdir: assets_subdir.into(),
            namespaces: HashMap::new(),
        }
    }

    /// Register a namespace manually (when NOT using Cargo.toml + build.rs).
    pub fn namespace(
        mut self,
        name: &str,
        dev: impl Into<String>,
        prod: impl Into<String>,
    ) -> Self {
        self.namespaces
            .insert(name.to_string(), Namespace { dev: dev.into(), prod: prod.into() });
        self
    }

    /// Resolve `"path"` or `"NS::path"`.
    pub fn resolve(&self, path_or_ns: &str) -> Option<PathBuf> {
        if let Some((ns, rel)) = path_or_ns.split_once("::") {
            return self.resolve_ns(ns, rel);
        }
        self.candidates(path_or_ns).into_iter().find(|p| p.exists())
    }

    /// 纯查询，无文件系统副作用（不建目录）。建目录是 [`Self::write`] 的职责——
    /// 否则 `exists()` / `read()` 这类存在性检查会凭空创建 namespace 目录。
    fn resolve_ns(&self, ns: &str, rel: &str) -> Option<PathBuf> {
        let cfg = self.namespaces.get(ns)?;
        let root = if is_dev() {
            self.manifest_dir.join(&cfg.dev)
        } else {
            expand_tilde(&cfg.prod)
        };
        Some(root.join(rel))
    }

    pub fn candidates(&self, rel: &str) -> Vec<PathBuf> {
        let mut v = Vec::with_capacity(3);
        v.push(self.manifest_dir.join(&self.assets_subdir).join(rel));
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                v.push(exe_dir.join(&self.assets_subdir).join(rel));
            }
        }
        v.push(Path::new(&self.assets_subdir).join(rel));
        v
    }

    /// Namespace dev + prod 两个根与 `rel` 拼接的候选（不做存在性检查）——调用方自行挑
    /// 存在的（如 `easytidy_core::server_binary_path`）。dev 根相对 manifest，prod 根做 `~/`
    /// 展开。镜像 [`Self::candidates`] 的「返回候选、不判存在」语义；纯函数，无建目录副作用。
    pub fn ns_candidates(&self, ns: &str, rel: &str) -> Vec<PathBuf> {
        let mut v = Vec::with_capacity(2);
        if let Some(n) = self.namespaces.get(ns) {
            v.push(self.manifest_dir.join(&n.dev).join(rel));
            v.push(expand_tilde(&n.prod).join(rel));
        }
        v
    }

    pub fn read(&self, path_or_ns: &str) -> Result<Vec<u8>> {
        let path = self.resolve(path_or_ns).ok_or_else(|| {
            // NS 路径（NS::rel）列真实 dev/prod 两个根；裸路径才列 assets 候选
            // （否则 `CONF::x.toml` 会被当裸相对路径拼进 assets，输出无意义路径）。
            let tried: String = match path_or_ns.split_once("::") {
                Some((ns, rel)) => self
                    .ns_candidates(ns, rel)
                    .iter()
                    .map(|p| format!("  {}", p.display()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => self
                    .candidates(path_or_ns)
                    .iter()
                    .map(|p| format!("  {}", p.display()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            anyhow::anyhow!("file not found: {path_or_ns}\ntried:\n{tried}")
        })?;
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))
    }

    pub fn read_str(&self, path_or_ns: &str) -> Result<String> {
        let bytes = self.read(path_or_ns)?;
        String::from_utf8(bytes).context("file is not valid UTF-8")
    }

    pub fn write(&self, path_or_ns: &str, data: &[u8]) -> Result<()> {
        let path = match self.resolve(path_or_ns) {
            Some(p) => p,
            None => self
                .candidates(path_or_ns)
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no candidate for {path_or_ns}"))?,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create parent {}", parent.display()))?;
        }
        std::fs::write(&path, data).with_context(|| format!("write {}", path.display()))
    }

    pub fn write_str(&self, path_or_ns: &str, contents: &str) -> Result<()> {
        self.write(path_or_ns, contents.as_bytes())
    }

    pub fn exists(&self, path_or_ns: &str) -> bool {
        self.resolve(path_or_ns).is_some()
    }
}

// 注：FileLoader **故意不实现 Default**——`env!("CARGO_MANIFEST_DIR")` 对调用方
// crate 而言是 shared **自己**的目录，`FileLoader::default()` 在别的 crate 里会
// 拿到错误的路径基准。调用方一律用 `loader!()` 宏（它用调用方自己的 manifest dir）。

/// Build a [`FileLoader`] for the **calling crate's** `assets/`, auto-loading namespaces
/// declared in `[package.metadata.shared]` (requires the crate to have a `build.rs` that calls
/// [`emit_namespaces`]). For bare-path assets only (no namespaces), it works without build.rs
/// (the generated file defaults to an empty table).
///
/// The calling code NEVER sees path values — only namespace names (`"CONF_DIR::my.conf"`).
#[macro_export]
macro_rules! loader {
    () => {{
        const __NS: &[(&str, &str, &str)] =
            include!(concat!(env!("OUT_DIR"), "/shared_ns.rs"));
        let mut __l = $crate::FileLoader::new(env!("CARGO_MANIFEST_DIR"), "assets");
        for &(__n, __d, __p) in __NS {
            __l = __l.namespace(__n, __d, __p);
        }
        __l
    }};
    ($sub:literal) => {{
        const __NS: &[(&str, &str, &str)] =
            include!(concat!(env!("OUT_DIR"), "/shared_ns.rs"));
        let mut __l = $crate::FileLoader::new(env!("CARGO_MANIFEST_DIR"), $sub);
        for &(__n, __d, __p) in __NS {
            __l = __l.namespace(__n, __d, __p);
        }
        __l
    }};
}

// ── build.rs helper: read Cargo.toml namespaces → generate shared_ns.rs ──────

/// Call from a crate's `build.rs`: reads `[package.metadata.shared]` from this crate's
/// `Cargo.toml`, generates `{OUT_DIR}/shared_ns.rs` (a const table consumed by [`loader!`]).
///
/// If no namespaces are declared, generates an empty table (so `loader!()` still compiles).
///
/// ```ignore
/// // build.rs
/// fn main() { shared::emit_namespaces(); }
/// ```
pub fn emit_namespaces() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR not set (must be called from build.rs)");
    let out_dir =
        std::env::var("OUT_DIR").expect("OUT_DIR not set (must be called from build.rs)");
    let cargo_toml = Path::new(&manifest_dir).join("Cargo.toml");
    let namespaces = match parse_metadata(&cargo_toml) {
        None => Vec::new(), // 无 [package.metadata.shared] 段——正常，空表
        Some(namespaces) if namespaces.is_empty() => {
            // 段头存在但一条没解析出 → 极可能是用了点号写法（`KEY.dev = "..."`）
            // 而非 inline 表（`KEY = { dev = "...", prod = "..." }`）。静默失败会
            // 让 `loader!()` 编译成空 namespace、运行时报 file not found，根因极难
            // 暴露——build 期告警。
            println!("cargo:warning=[package.metadata.shared] 段存在但未解析出任何 \
                      namespace——确认用 inline 表格式（KEY = {{ dev = \"...\", prod = \
                      \"...\" }}）；点号写法（KEY.dev = \"...\"）不被支持");
            namespaces
        }
        Some(namespaces) => namespaces,
    };
    // Generate the Rust source.
    let mut code = String::from("// AUTO-GENERATED by shared::emit_namespaces() — do not edit.\n&[\n");
    for (name, dev, prod) in &namespaces {
        code.push_str(&format!(
            "    (\"{}\", \"{}\", \"{}\"),\n",
            escape(name),
            escape(dev),
            escape(prod)
        ));
    }
    code.push_str("]\n");
    let dest = Path::new(&out_dir).join("shared_ns.rs");
    std::fs::write(&dest, code)
        .unwrap_or_else(|e| panic!("write {}: {e}", dest.display()));
    println!("cargo:rerun-if-changed=Cargo.toml");
}

/// Parse `[package.metadata.shared]` from Cargo.toml.
/// Returns `None` = 无段头；`Some(vec)` = 有段头（可能为空 vec）。
/// Naive scanner — handles the inline-table format:
/// ```toml
/// [package.metadata.shared]
/// CONF_DIR = { dev = "data/conf", prod = "~/.my-app/conf" }
/// ```
/// The header is matched at LINE START (not `find` anywhere) — a comment mentioning
/// `[package.metadata.shared]` must not hijack the scan (2026-08-19 dp-models 踩坑)。
fn parse_metadata(cargo_toml: &Path) -> Option<Vec<(String, String, String)>> {
    let Ok(content) = std::fs::read_to_string(cargo_toml) else {
        return None;
    };
    let lines: Vec<&str> = content.lines().collect();
    // Find the line whose trimmed start IS the section header (comments don't match).
    let Some(marker_idx) = lines
        .iter()
        .position(|l| l.trim_start().starts_with("[package.metadata.shared]"))
    else {
        return None; // no namespace section
    };
    // Lines until the next `[...]` section header.
    let section: Vec<&str> = lines[marker_idx + 1..]
        .iter()
        .take_while(|l| !l.trim_start().starts_with('['))
        .copied()
        .collect();
    // Parse lines like:  KEY = { dev = "...", prod = "..." }
    let mut result = Vec::new();
    for line in section {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, val)) = parse_inline_table(line) {
            result.push((name, val.0, val.1));
        }
    }
    Some(result)
}

/// Parse `KEY = { dev = "a", prod = "b" }` → `(KEY, (dev, prod))`.
fn parse_inline_table(line: &str) -> Option<(String, (String, String))> {
    let (key, rest) = line.split_once('=')?;
    let name = key.trim().to_string();
    // Extract quoted values for dev and prod from the inline table.
    let dev = extract_quoted(rest, "dev")?;
    let prod = extract_quoted(rest, "prod")?;
    Some((name, (dev, prod)))
}

/// Find `key = "value"` within `hay` and return the value.
fn extract_quoted(hay: &str, key: &str) -> Option<String> {
    let needle = format!("{key} = \"");
    let start = hay.find(&needle)? + needle.len();
    let rest = &hay[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ── dev/prod detection + data_dir ──────────────────────────────────────────────

pub fn is_dev() -> bool {
    std::env::var("CARGO_MANIFEST_DIR").is_ok()
}

pub fn data_dir(app_key: &str) -> Result<PathBuf> {
    let env_key = format!("{}_DATA_DIR", app_key.to_uppercase().replace('-', "_"));
    if let Ok(d) = std::env::var(&env_key) {
        let p = PathBuf::from(d);
        std::fs::create_dir_all(&p)
            .with_context(|| format!("create data dir {} (from {env_key})", p.display()))?;
        return Ok(p);
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(manifest).join("data");
        std::fs::create_dir_all(&p)?;
        return Ok(p);
    }
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .map_err(|_| anyhow::anyhow!("neither XDG_DATA_HOME nor HOME set"))?;
    let p = base.join(app_key);
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_with_home(path, std::env::var("HOME").ok())
}

/// `expand_tilde` 的可注入版本（测试传固定 home，避免改全局 `HOME` 引发
/// 多线程测试竞态）。
fn expand_tilde_with_home(path: &str, home: Option<String>) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home {
            return PathBuf::from(home).join(rest);
        }
    }
    if path == "~" {
        if let Some(home) = home {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}

// ── logging: process-wide tracing subscriber init (binaries only; `log` feature) ──

/// Initialize the process-wide `tracing` subscriber with the default filter `"info"`.
/// See [`init_tracing_with_filter`] for the full contract. Call ONCE, at the very top of a
/// binary's `main`.
#[cfg(feature = "log")]
pub fn init_tracing() {
    let _ = init_tracing_with_filter("info");
}

/// Initialize the process-wide `tracing` subscriber with a caller-supplied default filter
/// (typically from the app's config file, e.g. aura.yaml `log_level`). Call ONCE at the very
/// top of a binary's `main` (init-stage side effect; lib crates only use the `tracing` facade
/// macros). Returns the filter that actually took effect (e.g. `"info (fallback)"`,
/// `"RUST_LOG env (…)"`) so the caller can log the truth, not the configured intent.
///
/// - **Dev builds** (`debug_assertions`): human-readable colored output.
/// - **Release builds**: one JSON object per line — machine-parseable for ELK/Loki.
/// - **Level control** precedence: `RUST_LOG` env filter (standard escape hatch, e.g.
///   `RUST_LOG=aura_daemon=trace,info`) > `default_filter` > `"info"`. An invalid
///   `default_filter` is reported to stderr (pre-subscriber, hence plain eprintln) and
///   falls back to `"info"`. Accepts a bare level (`debug`) or per-target directives
///   (`aura_daemon=trace,info`).
/// - **Writer**: stderr, always. Log rotation/shipping is the HOST's job
///   (journald / logrotate / docker log-driver) — the process never writes log files itself.
#[cfg(feature = "log")]
pub fn init_tracing_with_filter(default_filter: &str) -> String {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
    let (filter, effective) = match EnvFilter::try_from_default_env() {
        // RUST_LOG set — the standard escape hatch wins over the config file.
        Ok(f) => {
            let effective = format!("RUST_LOG env ({f})");
            (f, effective)
        }
        Err(_) => match parse_config_filter(default_filter) {
            Ok(f) => (f, default_filter.trim().to_string()),
            Err(e) => {
                // Must log pre-subscriber — plain stderr so the misconfiguration is visible.
                eprintln!(
                    "shared: invalid log filter {default_filter:?} ({e}) — falling back to info"
                );
                (EnvFilter::new("info"), "info (fallback)".to_string())
            }
        },
    };
    let registry = tracing_subscriber::registry().with(filter);
    if cfg!(debug_assertions) {
        registry.with(fmt::layer().with_writer(std::io::stderr)).init();
    } else {
        registry.with(fmt::layer().json().with_writer(std::io::stderr)).init();
    }
    effective
}

/// Validate a config-file filter value: either a bare level word (the common case) or a
/// directive string (contains `=`/`,`, e.g. `"aura_daemon=debug,info"`), which
/// `EnvFilter::try_new` then validates itself. A bare NON-level word must be rejected here:
/// `EnvFilter` would silently treat it as a target name and mute the entire process.
#[cfg(feature = "log")]
fn parse_config_filter(s: &str) -> Result<tracing_subscriber::EnvFilter, String> {
    use tracing_subscriber::EnvFilter;
    const LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error", "off"];
    let t = s.trim();
    let bare_level = LEVELS.iter().any(|l| t.eq_ignore_ascii_case(l));
    if !bare_level && !t.contains('=') && !t.contains(',') {
        return Err(format!(
            "{t:?} is not a log level (trace|debug|info|warn|error|off) nor a directive"
        ));
    }
    EnvFilter::try_new(t).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_asset_resolves_in_source_tree() {
        let loader = FileLoader::new(env!("CARGO_MANIFEST_DIR"), "src");
        assert!(loader.exists("lib.rs"));
    }

    #[cfg(feature = "log")]
    #[test]
    fn config_filter_bare_words_and_directives_parse() {
        // Bare level words (case-insensitive) and directive strings are accepted…
        for ok in ["info", "DEBUG", " warn ", "trace", "error", "off", "aura_daemon=debug,info"] {
            assert!(parse_config_filter(ok).is_ok(), "{ok:?} should parse");
        }
        // …but a bare NON-level word must be REJECTED — EnvFilter would treat it as a target
        // name and silently mute the whole process (the "bogus!!" footgun).
        for bad in ["bogus!!", "infos", ""] {
            assert!(parse_config_filter(bad).is_err(), "{bad:?} should be rejected");
        }
        // Directives with a bad level still error via EnvFilter itself.
        assert!(parse_config_filter("aura_daemon=bogus").is_err());
    }

    #[test]
    fn bare_asset_not_found_lists_candidates() {
        let loader = FileLoader::new(env!("CARGO_MANIFEST_DIR"), "assets");
        let err = loader.read_str("__nope__.xyz").unwrap_err();
        assert!(format!("{err}").contains("tried:"));
    }

    #[test]
    fn namespace_resolves() {
        let loader = FileLoader::new(env!("CARGO_MANIFEST_DIR"), "assets")
            .namespace("CONF", "data/conf", "~/.my-app/conf");
        let path = loader.resolve("CONF::x.toml").unwrap();
        assert!(path.ends_with("data/conf/x.toml"));
    }

    #[test]
    fn namespace_unknown_returns_none() {
        let loader = FileLoader::new(env!("CARGO_MANIFEST_DIR"), "assets");
        assert!(loader.resolve("NOPE::x").is_none());
    }

    #[test]
    fn ns_candidates_lists_dev_then_prod_roots() {
        let loader = FileLoader::new(env!("CARGO_MANIFEST_DIR"), "assets")
            .namespace("CONF", "data/conf", "~/fake-home/conf");
        let cands = loader.ns_candidates("CONF", "a.conf");
        assert_eq!(cands.len(), 2);
        assert!(cands[0].ends_with("data/conf/a.conf"));
        // 未知 namespace → 空候选
        assert!(loader.ns_candidates("NOPE", "a.conf").is_empty());
    }

    #[test]
    fn namespace_write_and_read_back() {
        let loader = FileLoader::new(env!("CARGO_MANIFEST_DIR"), "assets")
            .namespace("OUT", "data/test_output", "~/.my-app/out");
        loader.write("OUT::roundtrip.txt", b"hello").unwrap();
        assert_eq!(loader.read_str("OUT::roundtrip.txt").unwrap(), "hello");
        let _ = std::fs::remove_file(loader.resolve("OUT::roundtrip.txt").unwrap());
    }

    #[test]
    fn extract_quoted_finds_value() {
        assert_eq!(
            extract_quoted(r#"{ dev = "a/b", prod = "~/c" }"#, "dev"),
            Some("a/b".into())
        );
        assert_eq!(
            extract_quoted(r#"{ dev = "a/b", prod = "~/c" }"#, "prod"),
            Some("~/c".into())
        );
    }

    #[test]
    fn parse_inline_table_works() {
        let (name, (dev, prod)) =
            parse_inline_table(r#"CONF_DIR = { dev = "data/conf", prod = "~/.my-app/conf" }"#)
                .unwrap();
        assert_eq!(name, "CONF_DIR");
        assert_eq!(dev, "data/conf");
        assert_eq!(prod, "~/.my-app/conf");
    }

    #[test]
    fn parse_metadata_no_section() {
        // A Cargo.toml without [package.metadata.shared] → None.
        assert!(parse_metadata(Path::new("/dev/null")).is_none());
    }

    #[test]
    fn parse_metadata_comment_with_marker_does_not_hijack() {
        // A comment containing "[package.metadata.shared]" BEFORE the real section must not
        // be picked as the header (2026-08-19 dp-models 踩坑: 注释含 marker → find 命中注释)。
        let tmp = std::env::temp_dir().join("shared_ns_test_comment.toml");
        std::fs::write(
            &tmp,
            "# see [package.metadata.shared]\n\
             [package.metadata.shared]\n\
             MODELS = { dev = \"../../assets/models\", prod = \"~/.desk-pilot/models\" }\n",
         )
        .unwrap();
        let v = parse_metadata(&tmp).expect("section present");
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(
            v,
            vec![("MODELS".to_string(), "../../assets/models".to_string(), "~/.desk-pilot/models".to_string())]
        );
    }

    #[test]
    fn parse_metadata_section_present_but_empty_is_some_empty() {
        // 段头存在但内容不是 inline 表（如点号写法）→ Some(空 vec)，触发 build 期告警。
        let tmp = std::env::temp_dir().join("shared_ns_test_empty.toml");
        std::fs::write(
            &tmp,
            "[package.metadata.shared]\nCONF_DIR.dev = \"data/conf\"\n",
        )
        .unwrap();
        let v = parse_metadata(&tmp).expect("section present");
        let _ = std::fs::remove_file(&tmp);
        assert!(v.is_empty(), "点号写法不被识别 → 空 vec");
    }

    #[test]
    fn expand_tilde_works() {
        // 用 expand_tilde_with_home 注入 home（不改全局 HOME，避多线程竞态）
        assert_eq!(
            expand_tilde_with_home("~/foo", Some("/tmp/fake_home".into())),
            PathBuf::from("/tmp/fake_home/foo")
        );
        assert_eq!(
            expand_tilde_with_home("~", Some("/tmp/fake_home".into())),
            PathBuf::from("/tmp/fake_home")
        );
        assert_eq!(expand_tilde_with_home("/abs", None), PathBuf::from("/abs"));
        // 无 home 时 ~ 原样保留
        assert_eq!(expand_tilde_with_home("~", None), PathBuf::from("~"));
    }

    #[test]
    fn is_dev_true_under_cargo() {
        assert!(is_dev());
    }
}
