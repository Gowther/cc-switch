//! dsh（DeepSeek Harness）配置文件读写模块
//!
//! 处理 `$DSH_HOME`（默认 `~/.dsh`，解析见 `crate::settings::get_dsh_dir`）下
//! 两个 YAML 文件的读写。dsh 对两个文件均有 watcher，外部写入热生效。
//!
//! ## `settings.yaml`（settings namespace → 分节 的 map）
//!
//! ```yaml
//! llm-pi-ai:
//!   providers:
//!     my-gateway:
//!       apiKeyEnv: DSH_MY_GATEWAY_API_KEY   # 凭据引用名，密钥绝不落此文件
//!       api: openai-completions             # 仅 openai-completions / openai-responses / anthropic-messages
//!       baseURL: https://gateway.example/v1
//!       models:
//!         - id: my-model
//!           name: My Model
//!
//! agent-default-model:                      # 当前供应商/模型
//!   provider: my-gateway
//!   model: my-model
//!   reasoningEffort: high                   # 可选
//! ```
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
//! `apiKey`：写入 dsh 时拆出到 `.credentials.yaml` 的 `refs`，settings.yaml
//! 只写 `apiKeyEnv: <引用名>`；从 dsh 读入时反向物化（refs 取回密钥填回
//! `apiKey`，并保留 `apiKeyEnv` 键）。
//!
//! 与 hermes_config 的差异：dsh 的 settings.yaml 由 dsh 自身程序化生成
//! （无注释保留需求），且 provider 路径是嵌套 mapping（非扁平分节），
//! 因此本模块采用 整文档 read-modify-write（Value 级深合并 + 整体序列化），
//! 不复用 hermes 的文本级分节替换与顶层键去重逻辑；备份策略、写锁、
//! `atomic_write` 与保留数量（`effective_backup_retain_count`）完全对齐。

use crate::config::{atomic_write, get_app_config_dir};
use crate::error::AppError;
use crate::settings::{effective_backup_retain_count, get_dsh_dir};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// settings.yaml 中 provider 所在的 settings namespace
const LLM_PI_AI_KEY: &str = "llm-pi-ai";
/// settings.yaml 中当前供应商/模型所在的 settings namespace
const DEFAULT_MODEL_KEY: &str = "agent-default-model";

// ============================================================================
// Path Functions
// ============================================================================

/// 获取 dsh `settings.yaml` 路径（`<dsh_dir>/settings.yaml`）
pub fn get_dsh_settings_path() -> PathBuf {
    get_dsh_dir().join("settings.yaml")
}

/// 获取 dsh `.credentials.yaml` 路径（`<dsh_dir>/.credentials.yaml`）
pub fn get_dsh_credentials_path() -> PathBuf {
    get_dsh_dir().join(".credentials.yaml")
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

/// `agent-default-model` 分节（当前供应商/模型）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshDefaultModel {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

// ============================================================================
// Core YAML Read/Write
// ============================================================================

/// 读取 dsh `settings.yaml` 为 serde_yaml::Value
///
/// 文件不存在、为空或只含注释（解析为 Null）时返回空 Mapping；
/// 解析错误报 `AppError::Config`。
pub fn read_dsh_settings() -> Result<serde_yaml::Value, AppError> {
    let path = get_dsh_settings_path();
    if !path.exists() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }

    let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    if content.trim().is_empty() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }

    let value: serde_yaml::Value = serde_yaml::from_str(&content)
        .map_err(|e| AppError::Config(format!("Failed to parse dsh settings.yaml as YAML: {e}")))?;
    Ok(match value {
        serde_yaml::Value::Null => serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        other => other,
    })
}

/// 写入 dsh `settings.yaml`（写锁 + 写前备份 + atomic_write）
///
/// 内容与磁盘一致时 no-op（不备份、不写盘）。
pub fn write_dsh_settings(value: &serde_yaml::Value) -> Result<DshWriteOutcome, AppError> {
    let _guard = dsh_write_lock().lock()?;
    write_dsh_settings_locked(value)
}

/// Inner write helper — caller must already hold the write lock.
fn write_dsh_settings_locked(value: &serde_yaml::Value) -> Result<DshWriteOutcome, AppError> {
    let path = get_dsh_settings_path();
    let raw = if path.exists() {
        fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?
    } else {
        String::new()
    };

    let serialized = serde_yaml::to_string(value)
        .map_err(|e| AppError::Config(format!("Failed to serialize dsh settings.yaml: {e}")))?;

    if serialized == raw {
        return Ok(DshWriteOutcome::default());
    }

    let backup_path = if !raw.is_empty() {
        Some(create_dsh_backup("settings", &raw)?)
    } else {
        None
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }

    atomic_write(&path, serialized.as_bytes())?;

    log::debug!("dsh settings.yaml written to {:?}", path);
    Ok(DshWriteOutcome {
        backup_path: backup_path.map(|p| p.display().to_string()),
    })
}

// ============================================================================
// Backup & Cleanup
// ============================================================================

/// 备份策略完全复用 hermes_config 的做法：时间戳命名、同秒冲突追加计数后缀、
/// 写后备份清理。文件为 `<cc-switch 配置目录>/backups/dsh/dsh_{kind}_*.yaml`，
/// `kind` 区分 settings / credentials / cordis（`cordis.patch.yml`，见
/// `mcp::dsh`），清理按 kind 分别计数。
pub(crate) fn create_dsh_backup(kind: &str, source: &str) -> Result<PathBuf, AppError> {
    let backup_dir = get_app_config_dir().join("backups").join("dsh");
    fs::create_dir_all(&backup_dir).map_err(|e| AppError::io(&backup_dir, e))?;

    let base_id = format!("dsh_{kind}_{}", Local::now().format("%Y%m%d_%H%M%S"));
    let mut filename = format!("{base_id}.yaml");
    let mut backup_path = backup_dir.join(&filename);
    let mut counter = 1;

    while backup_path.exists() {
        filename = format!("{base_id}_{counter}.yaml");
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
            entry.file_name().to_string_lossy().starts_with(&prefix)
                && entry
                    .path()
                    .extension()
                    .map(|ext| ext == "yaml" || ext == "yml")
                    .unwrap_or(false)
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
// YAML Value Helpers
// ============================================================================

/// 取 settings.yaml 顶层的可变 Mapping；Null 归一化为空 Mapping。
/// 顶层是其他非标量类型（配置已损坏，dsh 自身也无法加载）时报 Config
/// 错误而不是覆盖，避免误毁用户数据。
fn settings_root_mut(value: &mut serde_yaml::Value) -> Result<&mut serde_yaml::Mapping, AppError> {
    if value.is_null() {
        *value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    match value {
        serde_yaml::Value::Mapping(mapping) => Ok(mapping),
        _ => Err(AppError::Config(
            "dsh settings.yaml top level must be a mapping".to_string(),
        )),
    }
}

/// 取 `parent[key]` 的可变 Mapping；缺失时新建，存在但不是 Mapping
/// （损坏/旧格式）时告警并重建为空 Mapping。
fn ensure_child_mapping<'a>(
    parent: &'a mut serde_yaml::Mapping,
    key: &str,
) -> &'a mut serde_yaml::Mapping {
    let yaml_key = serde_yaml::Value::String(key.to_string());
    let needs_reset = match parent.get(&yaml_key) {
        Some(value) => !value.is_mapping(),
        None => true,
    };
    if needs_reset {
        if parent.contains_key(&yaml_key) {
            log::warn!("dsh settings.yaml: '{key}' is not a mapping, resetting");
        }
        parent.insert(
            yaml_key.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    match parent.get_mut(&yaml_key) {
        Some(serde_yaml::Value::Mapping(mapping)) => mapping,
        _ => unreachable!("just inserted a mapping"),
    }
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
// Provider Functions
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

/// 获取全部供应商（`llm-pi-ai.providers`），每项 YAML → JSON，
/// 并从 `.credentials.yaml` 的 `refs` 物化 `apiKey`（保留 `apiKeyEnv` 键）。
pub fn get_providers() -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
    let settings = read_dsh_settings()?;
    let refs = read_credentials_refs()?;
    let mut map = serde_json::Map::new();

    let Some(providers) = settings
        .get(LLM_PI_AI_KEY)
        .and_then(|v| v.get("providers"))
        .and_then(|v| v.as_mapping())
    else {
        return Ok(map);
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
            Ok(mut json_val) => {
                materialize_api_key(&mut json_val, &refs);
                map.insert(key_str.to_string(), json_val);
            }
            Err(e) => {
                log::warn!("Failed to convert dsh provider '{key_str}' to JSON: {e}");
            }
        }
    }

    Ok(map)
}

/// 获取单个供应商
pub fn get_provider(key: &str) -> Result<Option<serde_json::Value>, AppError> {
    Ok(get_providers()?.get(key).cloned())
}

/// Upsert `llm-pi-ai.providers.<key>`。
///
/// `apiKey` 是 cc-switch 私有键：取出后写入 `.credentials.yaml` 的 `refs`
/// （键 = `apiKeyEnv`，配置未提供时按 provider key 生成），settings.yaml
/// 只写 `apiKeyEnv: <引用名>`；其余键原样 JSON → YAML 透传。
///
/// 整个读-改-写在写锁内完成，避免 TOCTOU。
pub fn set_provider(
    key: &str,
    provider_config: serde_json::Value,
) -> Result<DshWriteOutcome, AppError> {
    let _guard = dsh_write_lock().lock()?;

    let mut normalized = provider_config;

    // 拆出 cc-switch 私有的 apiKey —— 密钥绝不写进 settings.yaml
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
    let mut yaml_val = json_to_yaml(&normalized)?;

    let mut settings = read_dsh_settings()?;
    let root = settings_root_mut(&mut settings)?;
    let providers = {
        let llm = ensure_child_mapping(root, LLM_PI_AI_KEY);
        ensure_child_mapping(llm, "providers")
    };
    let yaml_key = serde_yaml::Value::String(key.to_string());

    if let Some(existing) = providers.get_mut(&yaml_key) {
        // Forward-compat：保留盘上存在但本次 payload 未提交的字段（dsh 可选
        // 字段很多，用户可能经 dsh 自己的 UI 设置过 retryPolicy / timeoutMs 等）
        if let (Some(existing_map), serde_yaml::Value::Mapping(new_map)) =
            (existing.as_mapping(), &mut yaml_val)
        {
            for (k, v) in existing_map {
                new_map.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        *existing = yaml_val;
    } else {
        providers.insert(yaml_key, yaml_val);
    }

    write_dsh_settings_locked(&settings)
}

/// 删除 `llm-pi-ai.providers.<key>`（不存在时 no-op）。
///
/// 若该 provider 的 `apiKeyEnv` 引用名没有被其他 provider 引用，
/// 连同 `.credentials.yaml` 的 `refs` 条目一起删除。
pub fn remove_provider(key: &str) -> Result<DshWriteOutcome, AppError> {
    let _guard = dsh_write_lock().lock()?;

    let mut settings = read_dsh_settings()?;
    let root = settings_root_mut(&mut settings)?;

    let yaml_key = serde_yaml::Value::String(key.to_string());
    let removed = {
        let Some(providers) = root
            .get_mut(LLM_PI_AI_KEY)
            .and_then(|v| v.as_mapping_mut())
            .and_then(|llm| llm.get_mut("providers"))
            .and_then(|v| v.as_mapping_mut())
        else {
            return Ok(DshWriteOutcome::default());
        };
        let Some(existing) = providers.get(&yaml_key) else {
            return Ok(DshWriteOutcome::default());
        };
        let env_name = existing
            .get("apiKeyEnv")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        providers.remove(yaml_key);
        // 删除后仍引用同一 apiKeyEnv 的其他 provider
        let still_used = env_name.as_ref().map(|name| {
            providers
                .iter()
                .any(|(_, v)| v.get("apiKeyEnv").and_then(|x| x.as_str()) == Some(name.as_str()))
        });
        (env_name, still_used)
    };

    if let (Some(env_name), Some(false)) = removed {
        let mut refs = read_credentials_refs()?;
        if refs.remove(serde_yaml::Value::String(env_name)).is_some() {
            write_credentials_refs_locked(&refs)?;
        }
    }

    write_dsh_settings_locked(&settings)
}

// ============================================================================
// Default Model (agent-default-model)
// ============================================================================

/// 读取 `agent-default-model` 分节；分节缺失或缺 provider/model 字段时
/// 返回 None（不报错）。
pub fn get_default_model() -> Result<Option<DshDefaultModel>, AppError> {
    let settings = read_dsh_settings()?;
    let Some(section) = settings.get(DEFAULT_MODEL_KEY).and_then(|v| v.as_mapping()) else {
        return Ok(None);
    };
    let provider = section.get("provider").and_then(|v| v.as_str());
    let model = section.get("model").and_then(|v| v.as_str());
    let (Some(provider), Some(model)) = (provider, model) else {
        return Ok(None);
    };
    let reasoning_effort = section
        .get("reasoningEffort")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(Some(DshDefaultModel {
        provider: provider.to_string(),
        model: model.to_string(),
        reasoning_effort,
    }))
}

/// 切换当前供应商：改写 `agent-default-model` 分节。
///
/// 模型取该 provider 的 `models[0].id`（不向后扫描空 id，与 hermes 的
/// apply_switch_defaults 一致）；provider 无 models 时跳过写入并
/// log::warn（不报错）。分节里已有的 `reasoningEffort` 原样保留。
pub fn set_default_model(provider_key: &str) -> Result<DshWriteOutcome, AppError> {
    let _guard = dsh_write_lock().lock()?;

    let mut settings = read_dsh_settings()?;

    let first_model_id = settings
        .get(LLM_PI_AI_KEY)
        .and_then(|v| v.get("providers"))
        .and_then(|v| v.get(provider_key))
        .and_then(|v| v.get("models"))
        .and_then(|v| v.as_sequence())
        .and_then(|seq| seq.first())
        .and_then(|m| m.get("id"))
        .and_then(|id| id.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let Some(model_id) = first_model_id else {
        log::warn!(
            "dsh provider '{provider_key}' has no models; skipping agent-default-model update"
        );
        return Ok(DshWriteOutcome::default());
    };

    let root = settings_root_mut(&mut settings)?;

    // 保留分节里已有的 reasoningEffort（若存在）
    let reasoning_effort = root
        .get(DEFAULT_MODEL_KEY)
        .and_then(|v| v.get("reasoningEffort"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut section = serde_yaml::Mapping::new();
    section.insert(
        serde_yaml::Value::String("provider".to_string()),
        serde_yaml::Value::String(provider_key.to_string()),
    );
    section.insert(
        serde_yaml::Value::String("model".to_string()),
        serde_yaml::Value::String(model_id),
    );
    if let Some(effort) = reasoning_effort {
        section.insert(
            serde_yaml::Value::String("reasoningEffort".to_string()),
            serde_yaml::Value::String(effort),
        );
    }

    root.insert(
        serde_yaml::Value::String(DEFAULT_MODEL_KEY.to_string()),
        serde_yaml::Value::Mapping(section),
    );

    write_dsh_settings_locked(&settings)
}

// ============================================================================
// Common Config Snippets
// ============================================================================
//
// dsh 的通用配置片段是 YAML 文本，顶层 mapping 深合并进 settings.yaml。
// `llm-pi-ai`（provider 管理）与 `agent-default-model`（默认模型切换）由
// 本模块的专用函数维护，snippet 不允许触碰。

/// 受保护的顶层键：snippet 含这些键时忽略并告警
const PROTECTED_TOP_LEVEL_KEYS: &[&str] = &[LLM_PI_AI_KEY, DEFAULT_MODEL_KEY];

fn is_protected_top_level_key(key: &serde_yaml::Value) -> bool {
    key.as_str()
        .map(|s| PROTECTED_TOP_LEVEL_KEYS.contains(&s))
        .unwrap_or(false)
}

/// 解析 snippet 为 YAML Mapping；空/纯注释 → 空 Mapping；
/// 顶层非 Mapping 报 Config 错误。
pub(crate) fn parse_common_config_snippet(snippet: &str) -> Result<serde_yaml::Mapping, AppError> {
    if snippet.trim().is_empty() {
        return Ok(serde_yaml::Mapping::new());
    }
    let value: serde_yaml::Value = serde_yaml::from_str(snippet)
        .map_err(|e| AppError::Config(format!("Failed to parse dsh common config: {e}")))?;
    match value {
        serde_yaml::Value::Mapping(mapping) => Ok(mapping),
        serde_yaml::Value::Null => Ok(serde_yaml::Mapping::new()),
        _ => Err(AppError::Config(
            "dsh common config snippet must be a YAML mapping".to_string(),
        )),
    }
}

/// 深合并：两边都是 Mapping 时逐键递归，否则整体替换。
fn deep_merge_yaml(target: &mut serde_yaml::Value, source: &serde_yaml::Value) {
    if let (serde_yaml::Value::Mapping(target_map), serde_yaml::Value::Mapping(source_map)) =
        (&mut *target, source)
    {
        for (key, value) in source_map {
            match target_map.get_mut(key) {
                Some(existing) => deep_merge_yaml(existing, value),
                None => {
                    target_map.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *target = source.clone();
    }
}

/// 按 snippet 的键路径递归精确移除：仅当现有值与 snippet 值相等时才移除，
/// 避免误删用户后改的内容；嵌套 Mapping 被掏空后连键一起移除。
fn remove_matching_yaml(target: &mut serde_yaml::Value, source: &serde_yaml::Value) {
    let (serde_yaml::Value::Mapping(target_map), serde_yaml::Value::Mapping(source_map)) =
        (&mut *target, source)
    else {
        return;
    };
    for (key, source_value) in source_map {
        let should_remove = match target_map.get_mut(key) {
            Some(existing) if existing.is_mapping() && source_value.is_mapping() => {
                remove_matching_yaml(existing, source_value);
                existing.as_mapping().map(|m| m.is_empty()).unwrap_or(false)
            }
            Some(existing) => existing == source_value,
            None => false,
        };
        if should_remove {
            target_map.remove(key);
        }
    }
}

/// settings 是否已包含 snippet 的所有键值（递归；Mapping 只要求子集）
fn yaml_contains(target: &serde_yaml::Value, source: &serde_yaml::Value) -> bool {
    match (target, source) {
        (serde_yaml::Value::Mapping(target_map), serde_yaml::Value::Mapping(source_map)) => {
            source_map.iter().all(|(key, value)| {
                target_map
                    .get(key)
                    .map(|existing| yaml_contains(existing, value))
                    .unwrap_or(false)
            })
        }
        (target, source) => target == source,
    }
}

/// 将通用配置片段（YAML mapping）逐顶层键深合并进 settings.yaml。
/// 保护键 `llm-pi-ai` / `agent-default-model` 不合并（忽略并 log::warn）。
pub fn apply_dsh_common_config(snippet: &str) -> Result<(), AppError> {
    let snippet_map = parse_common_config_snippet(snippet)?;
    if snippet_map.is_empty() {
        return Ok(());
    }

    let _guard = dsh_write_lock().lock()?;
    let mut settings = read_dsh_settings()?;
    let root = settings_root_mut(&mut settings)?;

    for (key, value) in &snippet_map {
        if is_protected_top_level_key(key) {
            log::warn!(
                "dsh common config: ignored protected top-level key '{}'",
                key.as_str().unwrap_or("<non-string>")
            );
            continue;
        }
        match root.get_mut(key) {
            Some(existing) => deep_merge_yaml(existing, value),
            None => {
                root.insert(key.clone(), value.clone());
            }
        }
    }

    write_dsh_settings_locked(&settings)?;
    Ok(())
}

/// 按 snippet 的键路径从 settings.yaml 精确移除（仅当现有值与 snippet
/// 值相等时才移除，避免误删用户后改的内容）。保护键同样不触碰。
pub fn remove_dsh_common_config(snippet: &str) -> Result<(), AppError> {
    let snippet_map = parse_common_config_snippet(snippet)?;
    if snippet_map.is_empty() {
        return Ok(());
    }

    let _guard = dsh_write_lock().lock()?;
    let mut settings = read_dsh_settings()?;
    let root = settings_root_mut(&mut settings)?;

    for (key, value) in &snippet_map {
        if is_protected_top_level_key(key) {
            log::warn!(
                "dsh common config: ignored protected top-level key '{}'",
                key.as_str().unwrap_or("<non-string>")
            );
            continue;
        }
        let should_remove = match root.get_mut(key) {
            Some(existing) if existing.is_mapping() && value.is_mapping() => {
                remove_matching_yaml(existing, value);
                existing.as_mapping().map(|m| m.is_empty()).unwrap_or(false)
            }
            Some(existing) => existing == value,
            None => false,
        };
        if should_remove {
            root.remove(key);
        }
    }

    write_dsh_settings_locked(&settings)?;
    Ok(())
}

/// settings.yaml 是否已包含 snippet 的所有键值（保护键不参与判定；
/// snippet 解析失败时返回 false）
pub fn dsh_common_config_applied(snippet: &str) -> bool {
    let Ok(snippet_map) = parse_common_config_snippet(snippet) else {
        return false;
    };
    if snippet_map.is_empty() {
        return true;
    }
    let Ok(settings) = read_dsh_settings() else {
        return false;
    };
    let Some(root) = settings.as_mapping() else {
        return false;
    };
    snippet_map
        .iter()
        .filter(|(key, _)| !is_protected_top_level_key(key))
        .all(|(key, value)| {
            root.get(key)
                .map(|existing| yaml_contains(existing, value))
                .unwrap_or(false)
        })
}

// ============================================================================
// Validation
// ============================================================================

/// dsh `llm-pi-ai` 允许的线协议（provider.ts 的 PROTOCOLS 表）
const ALLOWED_APIS: &[&str] = &[
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
];

/// 校验 dsh 供应商配置（对齐 dsh 对 catalog 未知自定义路由的硬性要求）：
/// `api`（若存在）必须在三枚举内；`baseURL` 必须是非空字符串；
/// `models` 必须是非空数组且元素有非空 `id`。
pub fn validate_dsh_provider_config(config: &serde_json::Value) -> Result<(), AppError> {
    let obj = config.as_object().ok_or_else(|| {
        AppError::localized(
            "provider.dsh.configNotObject",
            "dsh 供应商配置必须是 JSON 对象",
            "dsh provider config must be a JSON object.",
        )
    })?;

    if let Some(api) = obj.get("api") {
        let valid = api
            .as_str()
            .map(|s| ALLOWED_APIS.contains(&s))
            .unwrap_or(false);
        if !valid {
            return Err(AppError::localized(
                "provider.dsh.invalidApi",
                format!("api 必须是以下之一: {}", ALLOWED_APIS.join(", ")),
                format!("api must be one of: {}.", ALLOWED_APIS.join(", ")),
            ));
        }
    }

    let base_url_ok = obj
        .get("baseURL")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !base_url_ok {
        return Err(AppError::localized(
            "provider.dsh.baseUrlRequired",
            "baseURL 不能为空",
            "baseURL is required.",
        ));
    }

    match obj.get("models") {
        Some(serde_json::Value::Array(models)) if !models.is_empty() => {
            for (index, model) in models.iter().enumerate() {
                let id_ok = model
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                if !id_ok {
                    return Err(AppError::localized(
                        "provider.dsh.modelIdRequired",
                        format!("models[{index}] 缺少非空 id"),
                        format!("models[{index}] must have a non-empty id."),
                    ));
                }
            }
        }
        _ => {
            return Err(AppError::localized(
                "provider.dsh.modelsRequired",
                "models 必须是非空数组",
                "models must be a non-empty array.",
            ));
        }
    }

    Ok(())
}

// ============================================================================
// YAML ↔ JSON Conversion Helpers
// ============================================================================

/// Convert a `serde_yaml::Value` to a `serde_json::Value`.
pub(crate) fn yaml_to_json(yaml: &serde_yaml::Value) -> Result<serde_json::Value, AppError> {
    // Serialize YAML value to string, then parse as JSON value.
    // This handles all type mappings correctly.
    let yaml_str = serde_yaml::to_string(yaml)
        .map_err(|e| AppError::Config(format!("Failed to serialize YAML value: {e}")))?;
    serde_yaml::from_str::<serde_json::Value>(&yaml_str)
        .map_err(|e| AppError::Config(format!("Failed to convert YAML to JSON: {e}")))
}

/// Convert a `serde_json::Value` to a `serde_yaml::Value`.
pub(crate) fn json_to_yaml(json: &serde_json::Value) -> Result<serde_yaml::Value, AppError> {
    let json_str = serde_json::to_string(json)
        .map_err(|e| AppError::Config(format!("Failed to serialize JSON value: {e}")))?;
    serde_yaml::from_str(&json_str)
        .map_err(|e| AppError::Config(format!("Failed to convert JSON to YAML: {e}")))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::{Mutex, OnceLock};

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    /// Run a test with an isolated temp home directory.
    ///
    /// Saves and restores `CC_SWITCH_TEST_HOME` to avoid interfering with
    /// parallel tests in other modules, and neutralizes `DSH_HOME` so an
    /// ambient value (e.g. from a real dsh install) can't make tests escape
    /// the temp home.
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

    /// Seed a settings.yaml with the given raw content.
    fn seed_settings(raw: &str) {
        let path = get_dsh_settings_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, raw).unwrap();
    }

    // ---- generate_api_key_env_name ----

    #[test]
    fn api_key_env_name_from_plain_key() {
        assert_eq!(
            generate_api_key_env_name("deepseek"),
            "DSH_DEEPSEEK_API_KEY"
        );
    }

    #[test]
    fn api_key_env_name_folds_hyphens() {
        assert_eq!(
            generate_api_key_env_name("my-gateway"),
            "DSH_MY_GATEWAY_API_KEY"
        );
    }

    #[test]
    fn api_key_env_name_folds_spaces_and_symbols() {
        assert_eq!(
            generate_api_key_env_name("my gateway.v2"),
            "DSH_MY_GATEWAY_V2_API_KEY"
        );
    }

    #[test]
    fn api_key_env_name_keeps_uppercase_and_digits() {
        assert_eq!(
            generate_api_key_env_name("ALREADY2"),
            "DSH_ALREADY2_API_KEY"
        );
    }

    // ---- provider CRUD + credentials split ----

    #[test]
    #[serial]
    fn read_settings_returns_empty_mapping_when_missing() {
        with_test_home(|| {
            let value = read_dsh_settings().unwrap();
            assert!(value.as_mapping().unwrap().is_empty());
            assert!(get_providers().unwrap().is_empty());
        });
    }

    #[test]
    #[serial]
    fn provider_roundtrip_splits_api_key_into_credentials() {
        with_test_home(|| {
            let config = serde_json::json!({
                "api": "openai-completions",
                "baseURL": "https://api.example.com/v1",
                "apiKey": "sk-test-123",
                "displayName": "Demo",
                "timeoutMs": 60000,
                "compat": { "supportsDeveloperRole": false },
                "models": [{ "id": "model-a", "name": "Model A" }],
            });
            set_provider("demo", config).unwrap();

            // settings.yaml: apiKeyEnv 落盘，密钥绝不出现
            let raw = fs::read_to_string(get_dsh_settings_path()).unwrap();
            assert!(!raw.contains("sk-test-123"));
            let yaml: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
            let entry = yaml
                .get("llm-pi-ai")
                .and_then(|v| v.get("providers"))
                .and_then(|v| v.get("demo"))
                .unwrap();
            assert_eq!(
                entry.get("apiKeyEnv").and_then(|v| v.as_str()),
                Some("DSH_DEMO_API_KEY")
            );
            assert!(entry.get("apiKey").is_none());
            assert_eq!(
                entry.get("baseURL").and_then(|v| v.as_str()),
                Some("https://api.example.com/v1")
            );
            // 可选 passthrough 字段原样透传
            assert_eq!(entry.get("timeoutMs").and_then(|v| v.as_u64()), Some(60000));
            assert_eq!(
                entry
                    .get("compat")
                    .and_then(|v| v.get("supportsDeveloperRole"))
                    .and_then(|v| v.as_bool()),
                Some(false)
            );

            // credentials: version + refs
            let cred_raw = fs::read_to_string(get_dsh_credentials_path()).unwrap();
            let cred: serde_yaml::Value = serde_yaml::from_str(&cred_raw).unwrap();
            assert_eq!(cred.get("version").and_then(|v| v.as_u64()), Some(1));
            assert_eq!(
                cred.get("refs")
                    .and_then(|v| v.get("DSH_DEMO_API_KEY"))
                    .and_then(|v| v.as_str()),
                Some("sk-test-123")
            );

            // get_providers 反向物化 apiKey，保留 apiKeyEnv
            let providers = get_providers().unwrap();
            let demo = providers.get("demo").unwrap();
            assert_eq!(demo["apiKey"], "sk-test-123");
            assert_eq!(demo["apiKeyEnv"], "DSH_DEMO_API_KEY");
            assert_eq!(demo["baseURL"], "https://api.example.com/v1");
            assert_eq!(demo["models"][0]["id"], "model-a");
        });
    }

    #[test]
    #[serial]
    fn set_provider_respects_existing_api_key_env() {
        with_test_home(|| {
            let config = serde_json::json!({
                "baseURL": "https://api.example.com/v1",
                "apiKey": "sk-custom",
                "apiKeyEnv": "MY_CUSTOM_KEY",
                "models": [{ "id": "m" }],
            });
            set_provider("custom", config).unwrap();

            let settings = read_dsh_settings().unwrap();
            let entry = settings
                .get("llm-pi-ai")
                .and_then(|v| v.get("providers"))
                .and_then(|v| v.get("custom"))
                .unwrap();
            assert_eq!(
                entry.get("apiKeyEnv").and_then(|v| v.as_str()),
                Some("MY_CUSTOM_KEY")
            );

            let refs = read_credentials_refs().unwrap();
            assert_eq!(
                refs.get("MY_CUSTOM_KEY").and_then(|v| v.as_str()),
                Some("sk-custom")
            );
            assert!(refs.get("DSH_CUSTOM_API_KEY").is_none());

            let provider = get_provider("custom").unwrap().unwrap();
            assert_eq!(provider["apiKey"], "sk-custom");
            assert_eq!(provider["apiKeyEnv"], "MY_CUSTOM_KEY");
        });
    }

    #[test]
    #[serial]
    fn set_provider_without_api_key_writes_no_credentials() {
        with_test_home(|| {
            let config = serde_json::json!({
                "baseURL": "https://api.example.com/v1",
                "models": [{ "id": "m" }],
            });
            set_provider("plain", config).unwrap();
            // 无 apiKey：不生成 apiKeyEnv，也不创建 .credentials.yaml
            let settings = read_dsh_settings().unwrap();
            let entry = settings
                .get("llm-pi-ai")
                .and_then(|v| v.get("providers"))
                .and_then(|v| v.get("plain"))
                .unwrap();
            assert!(entry.get("apiKeyEnv").is_none());
            assert!(!get_dsh_credentials_path().exists());
        });
    }

    #[test]
    #[serial]
    fn set_provider_preserves_unknown_fields_on_update() {
        // dsh 的可选字段很多（retryPolicy / timeoutMs / ...），用户可能经 dsh
        // 自己的 UI 设置过；CC Switch 编辑其他字段时不得把它们抹掉。
        with_test_home(|| {
            seed_settings(
                "\
llm-pi-ai:
  providers:
    acme:
      apiKeyEnv: ACME_KEY
      baseURL: https://old.example.com
      retryPolicy:
        mode: normal
        maxRetries: 3
",
            );

            let update = serde_json::json!({ "baseURL": "https://new.example.com" });
            set_provider("acme", update).unwrap();

            let provider = get_provider("acme").unwrap().unwrap();
            assert_eq!(provider["baseURL"], "https://new.example.com");
            assert_eq!(provider["apiKeyEnv"], "ACME_KEY");
            assert_eq!(provider["retryPolicy"]["maxRetries"], 3);
        });
    }

    // ---- remove_provider refs cleanup ----

    #[test]
    #[serial]
    fn remove_provider_deletes_unreferenced_credential() {
        with_test_home(|| {
            set_provider(
                "solo",
                serde_json::json!({
                    "baseURL": "https://a.example.com",
                    "apiKey": "sk-solo",
                    "models": [{ "id": "m" }],
                }),
            )
            .unwrap();
            assert!(get_dsh_credentials_path().exists());

            remove_provider("solo").unwrap();

            assert!(get_providers().unwrap().is_empty());
            let refs = read_credentials_refs().unwrap();
            assert!(refs.get("DSH_SOLO_API_KEY").is_none());
        });
    }

    #[test]
    #[serial]
    fn remove_provider_keeps_shared_credential_until_last_reference() {
        with_test_home(|| {
            let mk = |base: &str, key: &str| {
                serde_json::json!({
                    "baseURL": base,
                    "apiKey": key,
                    "apiKeyEnv": "SHARED_KEY",
                    "models": [{ "id": "m" }],
                })
            };
            set_provider("one", mk("https://one.example.com", "sk-one")).unwrap();
            set_provider("two", mk("https://two.example.com", "sk-two")).unwrap();

            // 删除 one：SHARED_KEY 仍被 two 引用，refs 保留
            remove_provider("one").unwrap();
            let refs = read_credentials_refs().unwrap();
            assert_eq!(
                refs.get("SHARED_KEY").and_then(|v| v.as_str()),
                Some("sk-two")
            );
            assert!(get_provider("one").unwrap().is_none());
            assert!(get_provider("two").unwrap().is_some());

            // 删除 two：引用清零，refs 条目一并删除
            remove_provider("two").unwrap();
            let refs = read_credentials_refs().unwrap();
            assert!(refs.get("SHARED_KEY").is_none());
        });
    }

    #[test]
    #[serial]
    fn remove_provider_missing_is_noop() {
        with_test_home(|| {
            let outcome = remove_provider("ghost").unwrap();
            assert!(outcome.backup_path.is_none());
            assert!(!get_dsh_settings_path().exists());
        });
    }

    // ---- agent-default-model ----

    #[test]
    #[serial]
    fn default_model_roundtrip_with_models() {
        with_test_home(|| {
            assert!(get_default_model().unwrap().is_none());

            set_provider(
                "demo",
                serde_json::json!({
                    "baseURL": "https://a.example.com",
                    "models": [{ "id": "m1" }, { "id": "m2" }],
                }),
            )
            .unwrap();
            set_default_model("demo").unwrap();

            let dm = get_default_model().unwrap().unwrap();
            assert_eq!(dm.provider, "demo");
            assert_eq!(dm.model, "m1");
            assert!(dm.reasoning_effort.is_none());

            // 分节确实落盘
            let raw = fs::read_to_string(get_dsh_settings_path()).unwrap();
            assert!(raw.contains("agent-default-model"));
        });
    }

    #[test]
    #[serial]
    fn set_default_model_skips_provider_without_models() {
        with_test_home(|| {
            set_provider(
                "bare",
                serde_json::json!({ "baseURL": "https://a.example.com" }),
            )
            .unwrap();

            // 无 models：跳过写入，不报错
            set_default_model("bare").unwrap();
            assert!(get_default_model().unwrap().is_none());

            let raw = fs::read_to_string(get_dsh_settings_path()).unwrap();
            assert!(!raw.contains("agent-default-model"));
        });
    }

    #[test]
    #[serial]
    fn set_default_model_preserves_reasoning_effort() {
        with_test_home(|| {
            seed_settings(
                "\
llm-pi-ai:
  providers:
    old:
      baseURL: https://old.example.com
      models:
        - id: old-model
agent-default-model:
  provider: old
  model: old-model
  reasoningEffort: high
",
            );

            set_provider(
                "new",
                serde_json::json!({
                    "baseURL": "https://new.example.com",
                    "models": [{ "id": "new-model" }],
                }),
            )
            .unwrap();
            set_default_model("new").unwrap();

            let dm = get_default_model().unwrap().unwrap();
            assert_eq!(dm.provider, "new");
            assert_eq!(dm.model, "new-model");
            assert_eq!(dm.reasoning_effort.as_deref(), Some("high"));
        });
    }

    // ---- common config snippets ----

    #[test]
    #[serial]
    fn common_config_apply_contain_remove_roundtrip() {
        with_test_home(|| {
            let snippet = "\
agent:
  max_turns: 10
  temperature: 0.5
ui:
  theme: dark
";
            assert!(!dsh_common_config_applied(snippet));

            apply_dsh_common_config(snippet).unwrap();
            assert!(dsh_common_config_applied(snippet));

            let settings = read_dsh_settings().unwrap();
            let agent = settings.get("agent").unwrap();
            assert_eq!(agent.get("max_turns").and_then(|v| v.as_u64()), Some(10));
            assert_eq!(
                settings
                    .get("ui")
                    .and_then(|v| v.get("theme"))
                    .and_then(|v| v.as_str()),
                Some("dark")
            );

            remove_dsh_common_config(snippet).unwrap();
            assert!(!dsh_common_config_applied(snippet));
            let settings = read_dsh_settings().unwrap();
            assert!(settings.get("agent").is_none());
            assert!(settings.get("ui").is_none());
        });
    }

    #[test]
    #[serial]
    fn common_config_apply_deep_merges_with_existing() {
        with_test_home(|| {
            seed_settings("agent:\n  max_turns: 5\n");
            apply_dsh_common_config("agent:\n  temperature: 0.5\n").unwrap();

            let settings = read_dsh_settings().unwrap();
            let agent = settings.get("agent").unwrap();
            assert_eq!(agent.get("max_turns").and_then(|v| v.as_u64()), Some(5));
            assert_eq!(agent.get("temperature").and_then(|v| v.as_f64()), Some(0.5));
        });
    }

    #[test]
    #[serial]
    fn common_config_ignores_protected_keys() {
        with_test_home(|| {
            set_provider(
                "demo",
                serde_json::json!({
                    "baseURL": "https://a.example.com",
                    "models": [{ "id": "m" }],
                }),
            )
            .unwrap();

            let snippet = "\
llm-pi-ai:
  providers:
    evil:
      baseURL: https://evil.example.com
agent-default-model:
  provider: evil
  model: x
telemetry:
  enabled: false
";
            apply_dsh_common_config(snippet).unwrap();

            // 保护键未被合并
            assert!(get_provider("evil").unwrap().is_none());
            assert!(get_provider("demo").unwrap().is_some());
            assert!(get_default_model().unwrap().is_none());
            // 非保护键正常合并
            let settings = read_dsh_settings().unwrap();
            assert_eq!(
                settings
                    .get("telemetry")
                    .and_then(|v| v.get("enabled"))
                    .and_then(|v| v.as_bool()),
                Some(false)
            );
            // 保护键不参与 applied 判定
            assert!(dsh_common_config_applied(snippet));

            // remove 同样不触碰保护键
            remove_dsh_common_config(snippet).unwrap();
            assert!(get_provider("demo").unwrap().is_some());
            let settings = read_dsh_settings().unwrap();
            assert!(settings.get("telemetry").is_none());
        });
    }

    #[test]
    #[serial]
    fn common_config_remove_skips_user_modified_values() {
        with_test_home(|| {
            apply_dsh_common_config("ui:\n  theme: dark\n  font: mono\n").unwrap();

            // 用户随后改了 theme
            let mut settings = read_dsh_settings().unwrap();
            {
                let root = settings_root_mut(&mut settings).unwrap();
                let ui = ensure_child_mapping(root, "ui");
                ui.insert(
                    serde_yaml::Value::String("theme".to_string()),
                    serde_yaml::Value::String("light".to_string()),
                );
            }
            write_dsh_settings(&settings).unwrap();

            remove_dsh_common_config("ui:\n  theme: dark\n  font: mono\n").unwrap();

            let settings = read_dsh_settings().unwrap();
            let ui = settings.get("ui").unwrap();
            // theme 已被用户改走 → 不误删；font 值未变 → 移除
            assert_eq!(ui.get("theme").and_then(|v| v.as_str()), Some("light"));
            assert!(ui.get("font").is_none());
        });
    }

    #[test]
    fn common_config_snippet_must_be_mapping() {
        assert!(parse_common_config_snippet("- a\n- b\n").is_err());
        assert!(parse_common_config_snippet("").unwrap().is_empty());
        assert!(parse_common_config_snippet("# comment\n")
            .unwrap()
            .is_empty());
    }

    // ---- credentials file ----

    #[test]
    #[serial]
    fn credentials_preserve_records_and_permissions() {
        with_test_home(|| {
            let cred_path = get_dsh_credentials_path();
            fs::create_dir_all(cred_path.parent().unwrap()).unwrap();
            fs::write(
                &cred_path,
                "\
version: 1
refs:
  EXISTING_KEY: sk-old
records:
  llm-pi-ai/openai-codex:
    kind: grant
    payload:
      token: abc
",
            )
            .unwrap();

            set_provider(
                "demo",
                serde_json::json!({
                    "baseURL": "https://a.example.com",
                    "apiKey": "sk-new",
                    "models": [{ "id": "m" }],
                }),
            )
            .unwrap();

            let raw = fs::read_to_string(&cred_path).unwrap();
            let doc: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
            assert_eq!(doc.get("version").and_then(|v| v.as_u64()), Some(1));
            let refs = doc.get("refs").unwrap();
            assert_eq!(
                refs.get("EXISTING_KEY").and_then(|v| v.as_str()),
                Some("sk-old")
            );
            assert_eq!(
                refs.get("DSH_DEMO_API_KEY").and_then(|v| v.as_str()),
                Some("sk-new")
            );
            // records 原样保留
            assert_eq!(
                doc.get("records")
                    .and_then(|v| v.get("llm-pi-ai/openai-codex"))
                    .and_then(|v| v.get("kind"))
                    .and_then(|v| v.as_str()),
                Some("grant")
            );

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&cred_path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "credentials file must be 0600");
            }
        });
    }

    // ---- validate_dsh_provider_config ----

    #[test]
    fn validate_accepts_valid_config() {
        let config = serde_json::json!({
            "api": "openai-completions",
            "baseURL": "https://a.example.com",
            "models": [{ "id": "m" }],
        });
        assert!(validate_dsh_provider_config(&config).is_ok());
        // api 可省略（dsh 侧有 catalog 默认）
        let config = serde_json::json!({
            "baseURL": "https://a.example.com",
            "models": [{ "id": "m" }],
        });
        assert!(validate_dsh_provider_config(&config).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_api() {
        let config = serde_json::json!({
            "api": "bedrock",
            "baseURL": "https://a.example.com",
            "models": [{ "id": "m" }],
        });
        assert!(validate_dsh_provider_config(&config).is_err());
    }

    #[test]
    fn validate_rejects_missing_or_blank_base_url() {
        let config = serde_json::json!({ "models": [{ "id": "m" }] });
        assert!(validate_dsh_provider_config(&config).is_err());
        let config = serde_json::json!({ "baseURL": "  ", "models": [{ "id": "m" }] });
        assert!(validate_dsh_provider_config(&config).is_err());
    }

    #[test]
    fn validate_rejects_empty_models_and_missing_id() {
        let config = serde_json::json!({ "baseURL": "https://a.example.com" });
        assert!(validate_dsh_provider_config(&config).is_err());
        let config = serde_json::json!({ "baseURL": "https://a.example.com", "models": [] });
        assert!(validate_dsh_provider_config(&config).is_err());
        let config = serde_json::json!({
            "baseURL": "https://a.example.com",
            "models": [{ "name": "no-id" }],
        });
        assert!(validate_dsh_provider_config(&config).is_err());
    }

    // ---- yaml_to_json / json_to_yaml ----

    #[test]
    fn yaml_json_conversion_roundtrip() {
        let json = serde_json::json!({
            "name": "test",
            "count": 42,
            "nested": {
                "flag": true
            }
        });
        let yaml = json_to_yaml(&json).unwrap();
        let back = yaml_to_json(&yaml).unwrap();
        assert_eq!(json, back);
    }
}
