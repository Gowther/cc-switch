//! dsh（DeepSeek Harness）配置读写模块（cordis.patch.yml 版）
//!
//! 当前 dsh 把供应商/默认模型等 live 配置存在**每个 profile 的**
//! `~/.dsh/profiles/<profile>/cordis.patch.yml`（id 定向 patch 条目），
//! 不再使用 `~/.dsh/settings.yaml`（老版本会迁移走，留下
//! `settings.yaml.imported`）。凭据仍是 home 级全局文件
//! `~/.dsh/.credentials.yaml`（0600）。
//!
//! ## cordis.patch.yml 条目形态（dsh Web UI 自己写的就是这种）
//!
//! ```yaml
//! - id: llm-pi-ai
//!   name: '@deepseek-ai/dsh-llm-pi-ai'
//!   config:
//!     providers:
//!       my-gateway:
//!         apiKeyEnv: DSH_MY_GATEWAY_API_KEY   # 凭据引用名，密钥绝不落此文件
//!         api: openai-completions             # 仅 openai-completions / openai-responses / anthropic-messages
//!         baseURL: https://gateway.example/v1
//!         models:
//!           - id: my-model
//!             name: My Model
//!
//! - id: agent-default-model
//!   name: '@deepseek-ai/dsh-agent-default-model'
//!   config:
//!     provider: my-gateway                    # 当前供应商/模型
//!     model: my-model
//!     reasoningEffort: high                   # 可选
//! ```
//!
//! ## 写入策略
//!
//! - 供应商、默认模型、通用配置：写入**每个已有 profile** 的
//!   `cordis.patch.yml`（对齐 dsh UI「写进 active profile」的语义，且
//!   cc-switch 的供应商天然是全局概念）；一个 profile 都没有时（dsh 尚未
//!   运行过）回退写 home 级 `~/.dsh/cordis.patch.yml`（dsh 文档确认 home 级
//!   对所有 profile 生效）。
//! - 一个条目块内只改 `config.providers` / `agent-default-model` 的 config，
//!   条目级其他键（含 `name`）与其他条目块（含 `!!js` 值）逐字保留。
//! - 文本级条目块切分/重组复用 `mcp::dsh` 的共享原语（`split_entry_blocks` /
//!   `assemble_patch_text` / `parse_block_entry`）——serde_yaml 在 Value 层
//!   会丢 `!!js` 标签，非目标块绝不参与 Value 往返。
//!
//! ## `.credentials.yaml`（POSIX 下必须 0600）
//!
//! ```yaml
//! version: 1
//! refs:                                     # 引用名 → 密钥（apiKeyEnv 的值即此处的键）
//!   DSH_MY_GATEWAY_API_KEY: sk-...
//! records:                                  # OAuth/登录记录，本模块原样保留
//!   llm-pi-ai/openai-codex:
//!     kind: grant
//!     payload: { ... }
//! ```
//!
//! ## 与 cc-switch `Provider.settings_config`（JSON）的契约
//!
//! 键名与 dsh YAML 原生名一致（`api` / `baseURL` / `models[]` / `compat{}`
//! 等，外加可选 passthrough 字段原样透传）。唯一的例外是 cc-switch 私有键
//! `apiKey`：写入 dsh 时拆出到 `.credentials.yaml` 的 `refs`，patch 条目里
//! 只写 `apiKeyEnv: <引用名>`；从 dsh 读入时反向物化（refs 取回密钥填回
//! `apiKey`，并保留 `apiKeyEnv` 键）。

use crate::config::{atomic_write, get_app_config_dir};
use crate::error::AppError;
use crate::settings::{effective_backup_retain_count, get_dsh_dir};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// cordis.patch.yml 中 provider 所在的 patch 条目 id / 插件名
const LLM_PI_AI_ENTRY_ID: &str = "llm-pi-ai";
const LLM_PI_AI_PLUGIN: &str = "@deepseek-ai/dsh-llm-pi-ai";
/// 默认模型 patch 条目 id / 插件名
const DEFAULT_MODEL_ENTRY_ID: &str = "agent-default-model";
const DEFAULT_MODEL_PLUGIN: &str = "@deepseek-ai/dsh-agent-default-model";

// ============================================================================
// Path Functions
// ============================================================================

/// 获取 dsh 凭据文件路径（`<dsh_dir>/.credentials.yaml`）
pub fn get_dsh_credentials_path() -> PathBuf {
    get_dsh_dir().join(".credentials.yaml")
}

/// home 级 patch 文件（`~/.dsh/cordis.patch.yml`，对所有 profile 生效）
pub fn get_dsh_home_patch_path() -> PathBuf {
    get_dsh_dir().join("cordis.patch.yml")
}

/// 所有已有 profile 的 cordis.patch.yml 路径（profiles 目录下每个子目录
/// 都算一个 profile，patch 文件可尚不存在）。一个 profile 都没有时返回空表。
pub fn get_dsh_profile_patch_paths() -> Vec<PathBuf> {
    let profiles_dir = get_dsh_dir().join("profiles");
    let mut paths = Vec::new();
    if let Ok(entries) = fs::read_dir(&profiles_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                paths.push(path.join("cordis.patch.yml"));
            }
        }
    }
    paths.sort();
    paths
}

/// 写入目标 patch 文件列表：所有已有 profile；没有 profile 时回退 home 级。
fn patch_write_targets() -> Vec<PathBuf> {
    let profiles = get_dsh_profile_patch_paths();
    if profiles.is_empty() {
        vec![get_dsh_home_patch_path()]
    } else {
        profiles
    }
}

/// 读取目标 patch 文件列表：所有已存在的 profile patch + home 级（若存在）。
fn patch_read_sources() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = get_dsh_profile_patch_paths()
        .into_iter()
        .filter(|p| p.exists())
        .collect();
    let home = get_dsh_home_patch_path();
    if home.exists() {
        paths.push(home);
    }
    paths
}

/// dsh 是否已有任何形式的配置文件（任一 profile 的 patch / home patch /
/// 旧版 settings.yaml）
pub fn dsh_has_any_config() -> bool {
    get_dsh_home_patch_path().exists()
        || get_dsh_profile_patch_paths().iter().any(|p| p.exists())
        || get_dsh_dir().join("settings.yaml").exists()
}

fn dsh_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ============================================================================
// Type Definitions
// ============================================================================

/// dsh 写入结果
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DshWriteOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
}

/// dsh 默认模型（agent-default-model 条目）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshDefaultModel {
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

// ============================================================================
// Backup & Cleanup
// ============================================================================

/// 备份策略对齐 hermes_config：时间戳命名、同秒冲突追加计数后缀、写后清理。
/// 文件为 `<cc-switch 配置目录>/backups/dsh/dsh_{kind}_*`，`kind` 区分
/// patch（cordis.patch.yml）/ credentials（.credentials.yaml），分别计数。
pub(crate) fn create_dsh_backup(kind: &str, source: &str) -> Result<PathBuf, AppError> {
    let backup_dir = get_app_config_dir().join("backups").join("dsh");
    fs::create_dir_all(&backup_dir).map_err(|e| AppError::io(&backup_dir, e))?;

    let base_id = format!("dsh_{kind}_{}", Local::now().format("%Y%m%d_%H%M%S"));
    let ext = if kind == "credentials" { "yaml" } else { "yml" };
    let mut filename = format!("{base_id}.{ext}");
    let mut backup_path = backup_dir.join(&filename);
    let mut counter = 1;

    while backup_path.exists() {
        filename = format!("{base_id}_{counter}.{ext}");
        backup_path = backup_dir.join(&filename);
        counter += 1;
    }

    atomic_write(&backup_path, source.as_bytes())?;
    cleanup_dsh_backups(&backup_dir, kind)?;
    Ok(backup_path)
}

fn cleanup_dsh_backups(dir: &Path, kind: &str) -> Result<(), AppError> {
    let retain = effective_backup_retain_count();
    let prefix = format!("dsh_{kind}_");
    let mut entries = fs::read_dir(dir)
        .map_err(|e| AppError::io(dir, e))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(prefix.as_str())
        })
        .collect::<Vec<_>>();

    if entries.len() <= retain {
        return Ok(());
    }

    entries.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
    let remove_count = entries.len().saturating_sub(retain);
    for entry in entries.into_iter().take(remove_count) {
        if let Err(err) = fs::remove_file(entry.path()) {
            log::warn!(
                "Failed to remove old dsh config backup {}: {err}",
                entry.path().display()
            );
        }
    }

    Ok(())
}

// ============================================================================
// cordis.patch.yml 文本级块 IO（复用 mcp::dsh 的切分/重组原语）
// ============================================================================

use crate::mcp::dsh::{assemble_patch_text, parse_block_entry, split_entry_blocks};

/// 读取 patch 文件为 (preamble, 条目块原文列表)；文件不存在/空 → 空
fn read_patch_file(path: &Path) -> Result<(String, Vec<String>), AppError> {
    if !path.exists() {
        return Ok((String::new(), Vec::new()));
    }
    let content = fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    if content.trim().is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    split_entry_blocks(&content)
}

/// 写回 patch 文件（写前备份 + atomic_write；内容一致时 no-op；
/// 空内容且文件不存在时不创建）
fn write_patch_file(
    path: &Path,
    preamble: &str,
    blocks: &[String],
) -> Result<Option<PathBuf>, AppError> {
    let serialized = assemble_patch_text(preamble, blocks);

    if path.exists() {
        let existing = fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
        if existing == serialized {
            return Ok(None);
        }
        let backup = create_dsh_backup("patch", &existing)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
        }
        atomic_write(path, serialized.as_bytes())?;
        return Ok(Some(backup));
    }

    if serialized.trim().is_empty() {
        return Ok(None);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }
    atomic_write(path, serialized.as_bytes())?;
    Ok(None)
}

/// 条目块是否是指定 id 的**定向 patch 条目**（非 insert 条目）
fn block_is_config_entry(block_text: &str, entry_id: &str) -> bool {
    let Some(entry) = parse_block_entry(block_text) else {
        return false;
    };
    entry.get("insert").is_none() && entry.get("id").and_then(|v| v.as_str()) == Some(entry_id)
}

/// 把定向 patch 条目序列化为块文本（`- id: ...` 起，无尾换行）
fn serialize_config_entry(entry: &serde_yaml::Value) -> Result<String, AppError> {
    let text = serde_yaml::to_string(&serde_yaml::Value::Sequence(vec![entry.clone()]))
        .map_err(|e| AppError::Config(format!("Failed to serialize dsh patch entry: {e}")))?;
    Ok(text.trim_end().to_string())
}

/// 取定向条目的 config 可变引用
fn entry_config_mut(entry: &mut serde_yaml::Value) -> Result<&mut serde_yaml::Mapping, AppError> {
    let map = entry
        .as_mapping_mut()
        .ok_or_else(|| AppError::Config("dsh patch entry must be a mapping".to_string()))?;
    let config_key = serde_yaml::Value::String("config".to_string());
    if !map.contains_key(&config_key) {
        map.insert(
            config_key.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    map.get_mut(&config_key)
        .and_then(|v| v.as_mapping_mut())
        .ok_or_else(|| AppError::Config("dsh patch entry config must be a mapping".to_string()))
}

/// 在单个 patch 文件里 upsert/删除某个定向条目的"config 变换"
///
/// `entry_id`：条目 id（llm-pi-ai / agent-default-model）
/// `plugin`：新建条目时写入的 name 字段
/// `f`：对条目 config 的就地修改；返回 false 表示"该文件无此条目且无需创建"
/// （remove 路径），true 表示已修改/已创建。
/// 删除语义：若修改后条目只剩 id/name/空 config，则整块移除。
fn update_config_entry_in_file(
    path: &Path,
    entry_id: &str,
    plugin: &str,
    f: impl FnOnce(&mut serde_yaml::Mapping) -> Result<bool, AppError>,
) -> Result<bool, AppError> {
    let (preamble, mut blocks) = read_patch_file(path)?;

    for block in blocks.iter_mut() {
        if !block_is_config_entry(block, entry_id) {
            continue;
        }
        // 解析 → 改 → 重序列化该块（其余块原文不动，含 !!js 的用户条目安全）
        let mut entry = parse_block_entry(block).expect("block_is_config_entry checked");
        let changed = {
            let config = entry_config_mut(&mut entry)?;
            f(config)?
        };
        if !changed {
            return Ok(false);
        }
        // 条目空了（config 清空）→ 整块移除
        let config_empty = entry
            .get("config")
            .and_then(|v| v.as_mapping())
            .map(|m| m.is_empty())
            .unwrap_or(true);
        if config_empty {
            block.clear();
        } else {
            *block = serialize_config_entry(&entry)?;
        }
        blocks.retain(|b| !b.is_empty());
        write_patch_file(path, &preamble, &blocks)?;
        return Ok(true);
    }

    // 条目不存在：按 f 在新建条目上执行（f 自行决定是否有效）
    let mut entry = serde_yaml::Mapping::new();
    entry.insert(
        serde_yaml::Value::String("id".to_string()),
        serde_yaml::Value::String(entry_id.to_string()),
    );
    entry.insert(
        serde_yaml::Value::String("name".to_string()),
        serde_yaml::Value::String(plugin.to_string()),
    );
    let mut value = serde_yaml::Value::Mapping(entry);
    let changed = {
        let config = entry_config_mut(&mut value)?;
        f(config)?
    };
    if !changed {
        return Ok(false);
    }
    let config_empty = value
        .get("config")
        .and_then(|v| v.as_mapping())
        .map(|m| m.is_empty())
        .unwrap_or(true);
    if config_empty {
        return Ok(false);
    }
    blocks.push(serialize_config_entry(&value)?);
    write_patch_file(path, &preamble, &blocks)?;
    Ok(true)
}

/// 读取所有 patch 源里某条目的 config（首个命中的 profile 优先，home 兜底）
fn read_entry_config_from_sources(entry_id: &str) -> Result<Option<serde_yaml::Mapping>, AppError> {
    for path in patch_read_sources() {
        let (_preamble, blocks) = read_patch_file(&path)?;
        for block in &blocks {
            if block_is_config_entry(block, entry_id) {
                let entry = parse_block_entry(block).expect("checked");
                if let Some(config) = entry.get("config").and_then(|v| v.as_mapping()) {
                    return Ok(Some(config.clone()));
                }
                return Ok(Some(serde_yaml::Mapping::new()));
            }
        }
    }
    Ok(None)
}

// ============================================================================
// Credentials (~/.dsh/.credentials.yaml)
// ============================================================================

/// 读取 `.credentials.yaml` 全文（不存在/空 → 空 Mapping；解析错误报 Config）
fn read_credentials_doc() -> Result<serde_yaml::Mapping, AppError> {
    let path = get_dsh_credentials_path();
    if !path.exists() {
        return Ok(serde_yaml::Mapping::new());
    }

    let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    if content.trim().is_empty() {
        return Ok(serde_yaml::Mapping::new());
    }

    let value: serde_yaml::Value = serde_yaml::from_str(&content).map_err(|e| {
        AppError::Config(format!(
            "Failed to parse dsh .credentials.yaml as YAML: {e}"
        ))
    })?;
    match value {
        serde_yaml::Value::Mapping(mapping) => Ok(mapping),
        serde_yaml::Value::Null => Ok(serde_yaml::Mapping::new()),
        _ => Err(AppError::Config(
            "dsh .credentials.yaml top level must be a mapping".to_string(),
        )),
    }
}

/// 读取 `.credentials.yaml` 的 `refs` 节（引用名 → 密钥）
fn read_credentials_refs() -> Result<serde_yaml::Mapping, AppError> {
    let doc = read_credentials_doc()?;
    Ok(doc
        .get("refs")
        .and_then(|v| v.as_mapping())
        .cloned()
        .unwrap_or_default())
}

/// 以新 `refs` 覆盖写回 `.credentials.yaml`（写前备份 + atomic_write + 0600）。
///
/// 保持 `version: 1`（缺失时补上）与 `records`（OAuth 记录）原样；文件不
/// 存在时新建 `{version: 1, refs: {...}}`。dsh 会拒绝 group/other 可读的
/// 凭据文件，因此每次写入后都强制 0600。
///
/// Caller must already hold the write lock.
fn write_credentials_refs_locked(refs: &serde_yaml::Mapping) -> Result<(), AppError> {
    let path = get_dsh_credentials_path();
    let raw = if path.exists() {
        fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?
    } else {
        String::new()
    };

    let mut doc = read_credentials_doc()?;
    doc.insert(
        serde_yaml::Value::String("refs".to_string()),
        serde_yaml::Value::Mapping(refs.clone()),
    );
    if !doc.contains_key("version") {
        doc.insert(
            serde_yaml::Value::String("version".to_string()),
            serde_yaml::Value::Number(1.into()),
        );
    }

    let serialized = serde_yaml::to_string(&serde_yaml::Value::Mapping(doc))
        .map_err(|e| AppError::Config(format!("Failed to serialize dsh credentials: {e}")))?;

    if serialized == raw {
        // 内容未变，但仍顺手修复权限（dsh 拒绝非 0600 的凭据文件启动）
        if path.exists() {
            set_credentials_permissions(&path)?;
        }
        return Ok(());
    }

    if !raw.is_empty() {
        create_dsh_backup("credentials", &raw)?;
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }

    atomic_write(&path, serialized.as_bytes())?;
    set_credentials_permissions(&path)?;
    log::debug!("dsh .credentials.yaml written to {:?}", path);
    Ok(())
}

/// POSIX 下强制 0600（dsh 拒绝 group/other 可读的凭据文件）
#[cfg(unix)]
fn set_credentials_permissions(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| AppError::io(path, e))?
        .permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms).map_err(|e| AppError::io(path, e))
}

#[cfg(not(unix))]
fn set_credentials_permissions(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

// ============================================================================
// YAML <-> JSON helpers
// ============================================================================

pub(crate) fn yaml_to_json(yaml: &serde_yaml::Value) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(yaml)
        .map_err(|e| AppError::Config(format!("Failed to convert dsh YAML value to JSON: {e}")))
}

pub(crate) fn json_to_yaml(json: &serde_json::Value) -> Result<serde_yaml::Value, AppError> {
    serde_yaml::to_value(json)
        .map_err(|e| AppError::Config(format!("Failed to convert JSON value to dsh YAML: {e}")))
}

// ============================================================================
// Provider Functions（写入所有已有 profile 的 cordis.patch.yml）
// ============================================================================

/// 由 provider key 生成凭据引用名：`DSH_<PROVIDER_KEY>_API_KEY`
/// （provider key 转大写，非 `[A-Z0-9]` 字符逐个折叠为 `_`，
/// 例：`my-gateway` → `DSH_MY_GATEWAY_API_KEY`）。纯函数，便于单测。
fn generate_api_key_env_name(provider_key: &str) -> String {
    let mut name = String::with_capacity(provider_key.len() + "DSH__API_KEY".len());
    name.push_str("DSH_");
    for ch in provider_key.chars() {
        let upper = ch.to_ascii_uppercase();
        if upper.is_ascii_uppercase() || upper.is_ascii_digit() {
            name.push(upper);
        } else {
            name.push('_');
        }
    }
    name.push_str("_API_KEY");
    name
}

/// 从凭据 `refs` 取回 `apiKeyEnv` 对应的密钥，物化为 JSON 里的 `apiKey`
/// 键（cc-switch 私有键；`apiKeyEnv` 本身保留）。
fn materialize_api_key(config: &mut serde_json::Value, refs: &serde_yaml::Mapping) {
    let Some(obj) = config.as_object_mut() else {
        return;
    };
    let Some(env_name) = obj.get("apiKeyEnv").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(secret) = refs
        .get(env_name)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    obj.insert(
        "apiKey".to_string(),
        serde_json::Value::String(secret.to_string()),
    );
}

/// 从一份 patch 源的 llm-pi-ai 条目读出 providers（合并进 out；后读的
/// 同名 key 覆盖先读的——home 级 patch 最后读、优先级最高，与 dsh 的层叠
/// 语义一致）。
fn collect_providers_from_sources(
    out: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), AppError> {
    for path in patch_read_sources() {
        let Some(config) = read_entry_config_from_sources_at(&path, LLM_PI_AI_ENTRY_ID)? else {
            continue;
        };
        let Some(providers) = config.get("providers").and_then(|v| v.as_mapping()) else {
            continue;
        };
        for (key, value) in providers {
            let Some(key_str) = key.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            if !value.is_mapping() {
                log::debug!("Skipping dsh providers['{key_str}']: not a mapping");
                continue;
            }
            match yaml_to_json(value) {
                Ok(json_val) => {
                    out.insert(key_str.to_string(), json_val);
                }
                Err(err) => {
                    log::warn!(
                        "Skipping dsh provider '{key_str}' in {}: {err}",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(())
}

fn read_entry_config_from_sources_at(
    path: &Path,
    entry_id: &str,
) -> Result<Option<serde_yaml::Mapping>, AppError> {
    let (_preamble, blocks) = read_patch_file(path)?;
    for block in &blocks {
        if block_is_config_entry(block, entry_id) {
            let entry = parse_block_entry(block).expect("checked");
            if let Some(config) = entry.get("config").and_then(|v| v.as_mapping()) {
                return Ok(Some(config.clone()));
            }
            return Ok(Some(serde_yaml::Mapping::new()));
        }
    }
    Ok(None)
}

/// 获取全部供应商（所有 patch 源的 `llm-pi-ai` 条目 config.providers 合并），
/// 每项 YAML → JSON，并从 `.credentials.yaml` 的 `refs` 物化 `apiKey`
/// （保留 `apiKeyEnv` 键）。
pub fn get_providers() -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
    let mut map = serde_json::Map::new();
    collect_providers_from_sources(&mut map)?;
    let refs = read_credentials_refs()?;
    for value in map.values_mut() {
        materialize_api_key(value, &refs);
    }
    Ok(map)
}

/// 获取单个供应商（不存在返回 Ok(None)）
pub fn get_provider(key: &str) -> Result<Option<serde_json::Value>, AppError> {
    Ok(get_providers()?.remove(key))
}

/// Upsert 供应商：写入所有已有 profile 的 cordis.patch.yml（无 profile 时
/// 回退 home 级 `~/.dsh/cordis.patch.yml`）。
///
/// settings_config（JSON）→ `llm-pi-ai` 条目 `config.providers.<key>`：
/// `apiKey` 拆出写入 `.credentials.yaml` 的 `refs`（条目里只留 `apiKeyEnv`
/// 引用名）；其余键原样透传。盘上该 provider 已有条目的未知字段保留。
pub fn set_provider(
    key: &str,
    provider_config: serde_json::Value,
) -> Result<DshWriteOutcome, AppError> {
    let _guard = dsh_write_lock().lock()?;

    let mut normalized = provider_config;

    // 拆出 cc-switch 私有的 apiKey —— 密钥绝不写进 patch 文件
    let api_key = normalized
        .as_object_mut()
        .and_then(|obj| obj.remove("apiKey"))
        .and_then(|v| v.as_str().map(str::trim).map(str::to_string))
        .filter(|s| !s.is_empty());

    // 引用名：沿用配置里的 apiKeyEnv，否则（仅在带新密钥时）按 provider key 生成
    let env_name = normalized
        .get("apiKeyEnv")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| api_key.as_ref().map(|_| generate_api_key_env_name(key)));

    // 先写凭据 refs：失败时不留半成品 provider
    if let (Some(env_name), Some(secret)) = (&env_name, &api_key) {
        let mut refs = read_credentials_refs()?;
        refs.insert(
            serde_yaml::Value::String(env_name.clone()),
            serde_yaml::Value::String(secret.clone()),
        );
        write_credentials_refs_locked(&refs)?;
    }

    // 其余键原样透传；生成的新引用名需要补写进配置
    if let (Some(obj), Some(env_name)) = (normalized.as_object_mut(), &env_name) {
        obj.insert(
            "apiKeyEnv".to_string(),
            serde_json::Value::String(env_name.clone()),
        );
    }
    let provider_yaml = json_to_yaml(&normalized)?;

    let mut backup_path = None;
    for path in patch_write_targets() {
        let provider_yaml = provider_yaml.clone();
        let key_string = key.to_string();
        update_config_entry_in_file(&path, LLM_PI_AI_ENTRY_ID, LLM_PI_AI_PLUGIN, move |config| {
            // 取/建 config.providers map
            let providers_key = serde_yaml::Value::String("providers".to_string());
            if !config.contains_key(&providers_key) {
                config.insert(
                    providers_key.clone(),
                    serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                );
            }
            let providers = config
                .get_mut(&providers_key)
                .and_then(|v| v.as_mapping_mut())
                .ok_or_else(|| {
                    AppError::Config("dsh llm-pi-ai config.providers must be a mapping".to_string())
                })?;

            let yaml_key = serde_yaml::Value::String(key_string);
            // forward-compat：保留盘上该 provider 条目里本次未提交的字段
            if let (serde_yaml::Value::Mapping(new_map), Some(existing_map)) = (
                &provider_yaml,
                providers.get(&yaml_key).and_then(|v| v.as_mapping()),
            ) {
                let mut merged = new_map.clone();
                for (k, v) in existing_map {
                    merged.entry(k.clone()).or_insert_with(|| v.clone());
                }
                providers.insert(yaml_key, serde_yaml::Value::Mapping(merged));
                return Ok(true);
            }
            providers.insert(yaml_key, provider_yaml);
            Ok(true)
        })?;
        if backup_path.is_none() {
            // 记录首个写盘的备份（update_config_entry_in_file 内部已备份；
            // 这里只记录路径存在性用于 outcome——精确路径由调用方日志查看）
            backup_path = Some(path);
        }
    }

    Ok(DshWriteOutcome {
        backup_path: backup_path.map(|p| p.display().to_string()),
    })
}

/// 删除 `llm-pi-ai.providers.<key>`（所有 patch 目标；不存在时 no-op）。
///
/// 若该 provider 的 `apiKeyEnv` 引用名没有被其他 provider 引用，
/// 连同 `.credentials.yaml` 的 `refs` 条目一起删除。
pub fn remove_provider(key: &str) -> Result<DshWriteOutcome, AppError> {
    let _guard = dsh_write_lock().lock()?;

    // 先收集该 provider 用过的引用名（删除后查不到了）
    let removed_env_name = get_provider(key)?
        .and_then(|config| config.get("apiKeyEnv").cloned())
        .and_then(|v| v.as_str().map(str::to_string));

    let mut backup_path = None;
    for path in patch_write_targets() {
        let key_string = key.to_string();
        let changed = update_config_entry_in_file(
            &path,
            LLM_PI_AI_ENTRY_ID,
            LLM_PI_AI_PLUGIN,
            move |config| {
                let Some(providers) = config
                    .get_mut(serde_yaml::Value::String("providers".to_string()))
                    .and_then(|v| v.as_mapping_mut())
                else {
                    return Ok(false);
                };
                let removed = providers
                    .remove(serde_yaml::Value::String(key_string))
                    .is_some();
                Ok(removed)
            },
        )?;
        if changed && backup_path.is_none() {
            backup_path = Some(path);
        }
    }

    // 引用名不再被任何 provider 引用时清理凭据
    if let Some(env_name) = removed_env_name {
        let still_referenced = {
            let providers = get_providers()?;
            providers.values().any(|config| {
                config.get("apiKeyEnv").and_then(|v| v.as_str()) == Some(env_name.as_str())
            })
        };
        if !still_referenced {
            let mut refs = read_credentials_refs()?;
            refs.remove(serde_yaml::Value::String(env_name));
            write_credentials_refs_locked(&refs)?;
        }
    }

    Ok(DshWriteOutcome {
        backup_path: backup_path.map(|p| p.display().to_string()),
    })
}

// ============================================================================
// Default Model（agent-default-model 条目，写入所有已有 profile）
// ============================================================================

/// 读取当前默认模型（首个含该条目的 patch 源）
pub fn get_default_model() -> Result<Option<DshDefaultModel>, AppError> {
    let Some(config) = read_entry_config_from_sources(DEFAULT_MODEL_ENTRY_ID)? else {
        return Ok(None);
    };
    let provider = config
        .get("provider")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let model = config
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let reasoning_effort = config
        .get("reasoningEffort")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    match (provider, model) {
        (Some(provider), Some(model)) => Ok(Some(DshDefaultModel {
            provider,
            model,
            reasoning_effort,
        })),
        _ => Ok(None),
    }
}

/// 切换默认模型到指定 provider：所有已有 profile 的 `agent-default-model`
/// 条目改写为 `{provider: <key>, model: <该 provider 首个模型 id>}`。
/// 保留盘上已有的 `reasoningEffort`。provider 无 models 时跳过写入并告警。
pub fn set_default_model(provider_key: &str) -> Result<DshWriteOutcome, AppError> {
    let _guard = dsh_write_lock().lock()?;

    // 取该 provider 的首个模型 id
    let provider = get_provider(provider_key)?.ok_or_else(|| {
        AppError::localized(
            "dsh.default_model.provider_missing",
            format!("dsh 供应商不存在: {provider_key}"),
            format!("dsh provider does not exist: {provider_key}"),
        )
    })?;
    let model = provider
        .get("models")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let Some(model) = model else {
        log::warn!(
            "dsh provider '{provider_key}' has no models; skipping agent-default-model write"
        );
        return Ok(DshWriteOutcome::default());
    };

    let mut backup_path = None;
    for path in patch_write_targets() {
        let provider_string = provider_key.to_string();
        let model_string = model.clone();
        update_config_entry_in_file(
            &path,
            DEFAULT_MODEL_ENTRY_ID,
            DEFAULT_MODEL_PLUGIN,
            move |config| {
                // 保留盘上已有的 reasoningEffort
                config.insert(
                    serde_yaml::Value::String("provider".to_string()),
                    serde_yaml::Value::String(provider_string),
                );
                config.insert(
                    serde_yaml::Value::String("model".to_string()),
                    serde_yaml::Value::String(model_string),
                );
                Ok(true)
            },
        )?;
        if backup_path.is_none() {
            backup_path = Some(path);
        }
    }

    Ok(DshWriteOutcome {
        backup_path: backup_path.map(|p| p.display().to_string()),
    })
}

// ============================================================================
// Common Config Snippet（通用配置片段）
//
// dsh 的通用配置片段是 YAML mapping 文本，语义为 **llm-pi-ai 条目的 config
// 级共享默认值**（如 retryPolicy / timeoutMs / streamIdleTimeoutMs 等路由级
// 字段）——patch 模型下任意 settings namespace 没有统一的插件名可解析，因此
// 片段合并进所有已有 profile 的 `llm-pi-ai` 条目 config（`providers` 键由
// 供应商管理逻辑独占，不参与合并/移除/判定）。
// ============================================================================

const PROTECTED_SNIPPET_KEYS: &[&str] = &["providers"];

/// 解析通用配置片段（YAML 顶层必须是 mapping）
pub(crate) fn parse_common_config_snippet(snippet: &str) -> Result<serde_yaml::Mapping, AppError> {
    let trimmed = snippet.trim();
    if trimmed.is_empty() {
        return Ok(serde_yaml::Mapping::new());
    }
    let value: serde_yaml::Value = serde_yaml::from_str(trimmed).map_err(|e| {
        AppError::localized(
            "dsh_common_config_invalid",
            format!("无效的 dsh 通用配置 YAML: {e}"),
            format!("Invalid dsh common config YAML: {e}"),
        )
    })?;
    match value {
        serde_yaml::Value::Mapping(mapping) => Ok(mapping),
        serde_yaml::Value::Null => Ok(serde_yaml::Mapping::new()),
        _ => Err(AppError::localized(
            "dsh_common_config_invalid",
            "dsh 通用配置必须是 YAML mapping",
            "dsh common config must be a YAML mapping",
        )),
    }
}

fn deep_merge_yaml(target: &mut serde_yaml::Value, source: &serde_yaml::Value) {
    if let (serde_yaml::Value::Mapping(t), serde_yaml::Value::Mapping(s)) = (&mut *target, source) {
        for (key, value) in s {
            match t.get_mut(key) {
                Some(existing) => deep_merge_yaml(existing, value),
                None => {
                    t.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *target = source.clone();
    }
}

fn yaml_contains(target: &serde_yaml::Value, source: &serde_yaml::Value) -> bool {
    match (target, source) {
        (serde_yaml::Value::Mapping(t), serde_yaml::Value::Mapping(s)) => {
            s.iter().all(|(key, value)| {
                t.get(key)
                    .is_some_and(|target_value| yaml_contains(target_value, value))
            })
        }
        _ => target == source,
    }
}

fn remove_matching_yaml(target: &mut serde_yaml::Mapping, source: &serde_yaml::Mapping) {
    for (key, value) in source {
        let Some(existing) = target.get_mut(key) else {
            continue;
        };
        match (existing, value) {
            (serde_yaml::Value::Mapping(existing_map), serde_yaml::Value::Mapping(source_map)) => {
                remove_matching_yaml(existing_map, source_map);
                if existing_map.is_empty() {
                    target.remove(key);
                }
            }
            (existing, value) => {
                if existing == value {
                    target.remove(key);
                }
            }
        }
    }
}

/// 应用通用配置片段：深合并进所有已有 profile 的 `llm-pi-ai` 条目 config
/// （`providers` 保护键跳过并告警）
pub fn apply_dsh_common_config(snippet: &str) -> Result<(), AppError> {
    let snippet_map = parse_common_config_snippet(snippet)?;
    if snippet_map.is_empty() {
        return Ok(());
    }

    let _guard = dsh_write_lock().lock()?;
    for path in patch_write_targets() {
        let snippet_map = snippet_map.clone();
        update_config_entry_in_file(&path, LLM_PI_AI_ENTRY_ID, LLM_PI_AI_PLUGIN, move |config| {
            for (key, value) in &snippet_map {
                if key
                    .as_str()
                    .is_some_and(|k| PROTECTED_SNIPPET_KEYS.contains(&k))
                {
                    log::warn!("dsh 通用配置跳过保护键: providers");
                    continue;
                }
                match config.get_mut(key) {
                    Some(existing) => deep_merge_yaml(existing, value),
                    None => {
                        config.insert(key.clone(), value.clone());
                    }
                }
            }
            Ok(true)
        })?;
    }
    Ok(())
}

/// 移除通用配置片段（值已被用户改走的键不动）
pub fn remove_dsh_common_config(snippet: &str) -> Result<(), AppError> {
    let snippet_map = parse_common_config_snippet(snippet)?;
    if snippet_map.is_empty() {
        return Ok(());
    }

    let _guard = dsh_write_lock().lock()?;
    for path in patch_write_targets() {
        let snippet_map = snippet_map.clone();
        update_config_entry_in_file(&path, LLM_PI_AI_ENTRY_ID, LLM_PI_AI_PLUGIN, move |config| {
            let mut filtered = serde_yaml::Mapping::new();
            for (key, value) in &snippet_map {
                if key
                    .as_str()
                    .is_some_and(|k| PROTECTED_SNIPPET_KEYS.contains(&k))
                {
                    continue;
                }
                filtered.insert(key.clone(), value.clone());
            }
            remove_matching_yaml(config, &filtered);
            Ok(true)
        })?;
    }
    Ok(())
}

/// 判断片段内容是否已包含在**所有**已有 profile 的 `llm-pi-ai` 条目 config 里
pub fn dsh_common_config_applied(snippet: &str) -> bool {
    let Ok(snippet_map) = parse_common_config_snippet(snippet) else {
        return false;
    };
    let filtered: Vec<(&serde_yaml::Value, &serde_yaml::Value)> = snippet_map
        .iter()
        .filter(|(key, _)| {
            key.as_str()
                .is_some_and(|k| !PROTECTED_SNIPPET_KEYS.contains(&k))
        })
        .collect();
    if filtered.is_empty() {
        return false;
    }

    let targets = patch_read_sources();
    if targets.is_empty() {
        return false;
    }
    targets.iter().all(|path| {
        let Ok(Some(config)) = read_entry_config_from_sources_at(path, LLM_PI_AI_ENTRY_ID) else {
            return false;
        };
        filtered.iter().all(|(key, value)| {
            config
                .get(key)
                .is_some_and(|target| yaml_contains(target, value))
        })
    })
}

// ============================================================================
// Live 读取投影（services/provider/live.rs 的 read_live_settings 用）
// ============================================================================

/// 把 dsh live 配置投影为 JSON（形态对齐旧 settings.yaml：`{"llm-pi-ai":
/// {"providers": {...}, ...llm-pi-ai 其余 config 键}, "agent-default-model":
/// {...}}`）。不存在任何 patch 源时报 `dsh.config.missing`。
pub fn read_dsh_live_json() -> Result<serde_json::Value, AppError> {
    if patch_read_sources().is_empty() {
        return Err(AppError::localized(
            "dsh.config.missing",
            "dsh 配置文件不存在",
            "dsh configuration file not found",
        ));
    }

    let providers = get_providers()?;

    // llm-pi-ai 条目的其余 config 键（首个含该条目的源）
    let mut llm_extra = serde_json::Map::new();
    if let Some(config) = read_entry_config_from_sources(LLM_PI_AI_ENTRY_ID)? {
        for (key, value) in config {
            if key.as_str() == Some("providers") {
                continue;
            }
            if let (Some(key_str), Ok(json_val)) =
                (key.as_str().map(str::to_string), yaml_to_json(&value))
            {
                llm_extra.insert(key_str, json_val);
            }
        }
    }

    let mut llm = llm_extra;
    llm.insert(
        "providers".to_string(),
        serde_json::Value::Object(providers),
    );

    let mut root = serde_json::Map::new();
    root.insert(
        LLM_PI_AI_ENTRY_ID.to_string(),
        serde_json::Value::Object(llm),
    );

    if let Some(default_model) = get_default_model()? {
        let mut dm = serde_json::Map::new();
        dm.insert(
            "provider".to_string(),
            serde_json::Value::String(default_model.provider),
        );
        dm.insert(
            "model".to_string(),
            serde_json::Value::String(default_model.model),
        );
        if let Some(effort) = default_model.reasoning_effort {
            dm.insert(
                "reasoningEffort".to_string(),
                serde_json::Value::String(effort),
            );
        }
        root.insert(
            DEFAULT_MODEL_ENTRY_ID.to_string(),
            serde_json::Value::Object(dm),
        );
    }

    Ok(serde_json::Value::Object(root))
}

// ============================================================================
// Validation
// ============================================================================

const ALLOWED_APIS: &[&str] = &[
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
];

/// 校验扁平 settings_config：api 三枚举、baseURL 非空、models 非空且元素有 id
pub fn validate_dsh_provider_config(config: &serde_json::Value) -> Result<(), AppError> {
    let obj = config.as_object().ok_or_else(|| {
        AppError::localized(
            "provider.dsh.settings.not_object",
            "dsh 配置必须是 JSON 对象",
            "dsh configuration must be a JSON object",
        )
    })?;

    if let Some(api) = obj.get("api").and_then(|v| v.as_str()) {
        if !ALLOWED_APIS.contains(&api) {
            return Err(AppError::localized(
                "provider.dsh.api.invalid",
                format!(
                    "dsh 供应商 api '{api}' 非法；允许值: {}",
                    ALLOWED_APIS.join(", ")
                ),
                format!(
                    "Invalid dsh provider api '{api}'; allowed: {}",
                    ALLOWED_APIS.join(", ")
                ),
            ));
        }
    }

    let base_url = obj
        .get("baseURL")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if base_url.is_empty() {
        return Err(AppError::localized(
            "provider.dsh.base_url.missing",
            "dsh 供应商缺少 baseURL",
            "dsh provider is missing `baseURL`",
        ));
    }

    match obj.get("models").and_then(|v| v.as_array()) {
        Some(models) if !models.is_empty() => {
            for model in models {
                let valid = model
                    .get("id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| !id.trim().is_empty());
                if !valid {
                    return Err(AppError::localized(
                        "provider.dsh.models.missing_id",
                        "dsh 供应商 models 里的每个模型都必须有 id",
                        "Every model in dsh provider `models` must have an `id`",
                    ));
                }
            }
        }
        _ => {
            return Err(AppError::localized(
                "provider.dsh.models.empty",
                "dsh 供应商必须至少声明一个模型（models）",
                "dsh provider must declare at least one model (`models`)",
            ));
        }
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serial_test::serial;
    use std::sync::{Mutex, OnceLock};

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    /// 隔离临时 HOME：保存/恢复 CC_SWITCH_TEST_HOME，并中和 DSH_HOME
    /// （防止真实 dsh 安装把测试带出临时目录）。
    fn with_test_home<T>(test_fn: impl FnOnce() -> T) -> T {
        let _guard = test_guard();
        let tmp = tempfile::tempdir().unwrap();
        let old_test_home = std::env::var_os("CC_SWITCH_TEST_HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", tmp.path());
        let old_dsh_home = std::env::var_os("DSH_HOME");
        std::env::remove_var("DSH_HOME");
        let result = test_fn();
        match old_dsh_home {
            Some(value) => std::env::set_var("DSH_HOME", value),
            None => std::env::remove_var("DSH_HOME"),
        }
        match old_test_home {
            Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
        result
    }

    /// 建立 profiles/web 与 profiles/desktop 两个 profile 目录
    fn seed_profiles() -> (PathBuf, PathBuf) {
        let base = get_dsh_dir().join("profiles");
        let web = base.join("web").join("cordis.patch.yml");
        let desktop = base.join("desktop").join("cordis.patch.yml");
        fs::create_dir_all(web.parent().unwrap()).unwrap();
        fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        (web, desktop)
    }

    fn sample_config() -> serde_json::Value {
        json!({
            "api": "openai-completions",
            "baseURL": "https://api.example.com/v1",
            "apiKey": "sk-test",
            "models": [{ "id": "demo-model", "name": "Demo Model" }],
            "compat": { "supportsDeveloperRole": false }
        })
    }

    #[test]
    #[serial]
    fn provider_roundtrip_writes_all_profiles_and_splits_credentials() {
        with_test_home(|| {
            let (web, desktop) = seed_profiles();

            set_provider("demo", sample_config()).expect("set_provider");

            for path in [&web, &desktop] {
                let raw = fs::read_to_string(path).unwrap();
                assert!(raw.contains("id: llm-pi-ai"), "llm-pi-ai entry: {raw}");
                assert!(raw.contains("demo:"), "provider key: {raw}");
                assert!(raw.contains("apiKeyEnv: DSH_DEMO_API_KEY"), "{raw}");
                assert!(!raw.contains("sk-test"), "密钥不得写入 patch 文件");
            }

            // 凭据拆分到了 .credentials.yaml
            let creds = fs::read_to_string(get_dsh_credentials_path()).unwrap();
            assert!(creds.contains("version: 1"));
            assert!(creds.contains("DSH_DEMO_API_KEY: sk-test"));

            // 读回物化
            let providers = get_providers().unwrap();
            let demo = providers.get("demo").expect("demo provider");
            assert_eq!(demo.get("apiKey").and_then(|v| v.as_str()), Some("sk-test"));
            assert_eq!(
                demo.get("apiKeyEnv").and_then(|v| v.as_str()),
                Some("DSH_DEMO_API_KEY")
            );
            assert_eq!(
                demo.get("baseURL").and_then(|v| v.as_str()),
                Some("https://api.example.com/v1")
            );

            // 删除后 provider 与独占凭据引用都清除
            remove_provider("demo").unwrap();
            assert!(get_providers().unwrap().get("demo").is_none());
            let creds = fs::read_to_string(get_dsh_credentials_path()).unwrap();
            assert!(!creds.contains("DSH_DEMO_API_KEY"));
            // providers 表已掏空（llm-pi-ai 条目本身保留为 config: {providers: {}}，
            // 与 dsh UI 删除最后一个 provider 后的形态一致）
            let raw = fs::read_to_string(&web).unwrap();
            assert!(raw.contains("providers"), "{raw}");
            assert!(!raw.contains("demo:"), "{raw}");
        });
    }

    #[test]
    #[serial]
    fn provider_write_preserves_other_blocks_and_credentials_sections() {
        with_test_home(|| {
            let (web, _desktop) = seed_profiles();
            fs::write(
                &web,
                "# 用户头部注释\n- insert:\n    - id: user-plugin\n      config:\n        token: !!js process.env.MY_TOKEN\n",
            )
            .unwrap();
            fs::write(
                get_dsh_credentials_path(),
                "version: 1\nrefs:\n  OTHER_KEY: keep-me\nrecords:\n  some/thing:\n    kind: grant\n",
            )
            .unwrap();

            set_provider("demo", sample_config()).unwrap();

            let raw = fs::read_to_string(&web).unwrap();
            assert!(raw.contains("# 用户头部注释"));
            assert!(
                raw.contains("!!js process.env.MY_TOKEN"),
                "用户条目逐字保留"
            );
            let creds = fs::read_to_string(get_dsh_credentials_path()).unwrap();
            assert!(creds.contains("OTHER_KEY: keep-me"));
            assert!(creds.contains("records:"));
            assert!(creds.contains("some/thing:"));
        });
    }

    #[test]
    #[serial]
    fn default_model_set_get_and_preserve_reasoning_effort() {
        with_test_home(|| {
            let (web, _desktop) = seed_profiles();
            set_provider("demo", sample_config()).unwrap();
            // 预置 reasoningEffort
            fs::write(
                &web,
                "- id: agent-default-model\n  config:\n    provider: old\n    model: old-model\n    reasoningEffort: high\n",
            )
            .unwrap();

            set_default_model("demo").unwrap();

            let dm = get_default_model().unwrap().expect("default model");
            assert_eq!(dm.provider, "demo");
            assert_eq!(dm.model, "demo-model");

            let raw = fs::read_to_string(&web).unwrap();
            assert!(raw.contains("reasoningEffort: high"), "保留既有档位: {raw}");
        });
    }

    #[test]
    #[serial]
    fn default_model_skips_provider_without_models() {
        with_test_home(|| {
            seed_profiles();
            let mut config = sample_config();
            config.as_object_mut().unwrap().remove("models");
            set_provider("bare", config).unwrap();
            // 无 models：跳过写入，不报错
            set_default_model("bare").unwrap();
            assert!(get_default_model().unwrap().is_none());
        });
    }

    #[test]
    #[serial]
    fn common_config_apply_applied_remove_roundtrip() {
        with_test_home(|| {
            let (web, _desktop) = seed_profiles();
            let snippet =
                "retryPolicy:\n  mode: normal\n  maxRetries: 3\nstreamIdleTimeoutMs: 60000\n";

            assert!(!dsh_common_config_applied(snippet));
            apply_dsh_common_config(snippet).unwrap();
            assert!(dsh_common_config_applied(snippet));

            let raw = fs::read_to_string(&web).unwrap();
            assert!(raw.contains("retryPolicy"), "{raw}");
            assert!(raw.contains("streamIdleTimeoutMs: 60000"), "{raw}");

            remove_dsh_common_config(snippet).unwrap();
            assert!(!dsh_common_config_applied(snippet));
        });
    }

    #[test]
    #[serial]
    fn common_config_skips_protected_providers_key() {
        with_test_home(|| {
            seed_profiles();
            let snippet = "providers:\n  evil: {}\nstreamIdleTimeoutMs: 5000\n";
            apply_dsh_common_config(snippet).unwrap();
            let providers = get_providers().unwrap();
            assert!(providers.get("evil").is_none(), "providers 保护键不合并");
            let Some(config) = read_entry_config_from_sources(LLM_PI_AI_ENTRY_ID).unwrap() else {
                panic!("llm-pi-ai entry should exist");
            };
            assert!(config.get("streamIdleTimeoutMs").is_some());
        });
    }

    #[test]
    #[serial]
    fn write_without_profiles_falls_back_to_home_patch() {
        with_test_home(|| {
            // 无 profiles 目录 → 写 home 级 ~/.dsh/cordis.patch.yml
            set_provider("demo", sample_config()).unwrap();
            let home = get_dsh_home_patch_path();
            let raw = fs::read_to_string(&home).unwrap();
            assert!(raw.contains("id: llm-pi-ai"));
            assert!(raw.contains("demo:"));
        });
    }

    #[test]
    #[serial]
    fn write_creates_patch_backup_on_overwrite() {
        with_test_home(|| {
            let (web, _) = seed_profiles();
            set_provider("demo", sample_config()).unwrap();
            let mut changed = sample_config();
            changed["baseURL"] = json!("https://changed.example.com/v1");
            set_provider("demo", changed).unwrap();
            let backups = get_app_config_dir().join("backups").join("dsh");
            let count = fs::read_dir(&backups)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("dsh_patch_"))
                .count();
            assert!(count >= 1, "patch 备份应存在");
            let _ = web;
        });
    }

    #[test]
    fn validate_accepts_valid_config() {
        validate_dsh_provider_config(&json!({
            "api": "openai-completions",
            "baseURL": "https://x",
            "models": [{ "id": "m" }]
        }))
        .unwrap();
    }

    #[test]
    fn validate_rejects_bad_api_blank_base_url_empty_models() {
        assert!(validate_dsh_provider_config(&json!({
            "api": "grpc",
            "baseURL": "https://x",
            "models": [{ "id": "m" }]
        }))
        .unwrap_err()
        .to_string()
        .contains("api"));
        assert!(validate_dsh_provider_config(&json!({
            "api": "openai-completions",
            "models": [{ "id": "m" }]
        }))
        .unwrap_err()
        .to_string()
        .contains("baseURL"));
        assert!(validate_dsh_provider_config(&json!({
            "api": "openai-completions",
            "baseURL": "https://x",
            "models": []
        }))
        .unwrap_err()
        .to_string()
        .contains("models"));
        assert!(validate_dsh_provider_config(&json!({
            "api": "openai-completions",
            "baseURL": "https://x",
            "models": [{ "name": "no-id" }]
        }))
        .is_err());
    }

    #[test]
    fn api_key_env_name_generation() {
        assert_eq!(generate_api_key_env_name("demo"), "DSH_DEMO_API_KEY");
        assert_eq!(
            generate_api_key_env_name("my-gateway"),
            "DSH_MY_GATEWAY_API_KEY"
        );
        assert_eq!(generate_api_key_env_name("a b.c"), "DSH_A_B_C_API_KEY");
        assert_eq!(
            generate_api_key_env_name("ALREADY1"),
            "DSH_ALREADY1_API_KEY"
        );
    }

    #[test]
    fn read_live_json_shape() {
        with_test_home(|| {
            seed_profiles();
            set_provider("demo", sample_config()).unwrap();
            set_default_model("demo").unwrap();

            let live = read_dsh_live_json().unwrap();
            let providers = live["llm-pi-ai"]["providers"].as_object().unwrap();
            assert!(providers.contains_key("demo"));
            assert_eq!(live["agent-default-model"]["provider"], "demo");
            assert_eq!(live["agent-default-model"]["model"], "demo-model");
        });
    }
}
