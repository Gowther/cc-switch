//! Kimi Code CLI 配置文件读写模块
//!
//! 处理 `~/.kimi-code/config.toml`（TOML）的供应商与默认模型管理，以及
//! `~/.kimi-code/mcp.json`（JSON）的读-改-写入口。Kimi Code 使用累加式
//! 供应商管理：所有供应商共存于 `[providers."<name>"]` 表，模型别名在
//! `[models."<alias>"]` 表，当前模型由顶层 `default_model` 键决定。
//!
//! ## 配置结构示例
//!
//! ```toml
//! default_model = "ccs/my-relay"
//!
//! [providers."ccs-my-relay"]
//! type = "anthropic"
//! base_url = "https://api.example.com"
//! api_key = "sk-..."
//!
//! [models."ccs/my-relay"]
//! provider = "ccs-my-relay"
//! model = "claude-sonnet-5"
//! max_context_size = 1000000
//! ```
//!
//! cc-switch 管理的条目固定使用 `ccs-`（provider）/ `ccs/`（model alias）
//! 前缀，避免与 `/login` 创建的 `managed:kimi-code` 及用户手写条目冲突。
//! 其余条目、注释与格式由 toml_edit 原样保留。

use crate::config::{atomic_write, get_app_config_dir};
use crate::error::AppError;
use crate::settings::effective_backup_retain_count;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use toml_edit::{DocumentMut, Item, Table};

// ============================================================================
// Path Functions
// ============================================================================

/// 获取 Kimi Code 主配置文件路径（`<dir>/config.toml`）
pub fn get_kimi_code_config_path() -> PathBuf {
    crate::settings::get_kimi_code_dir().join("config.toml")
}

/// 获取 Kimi Code MCP 配置文件路径（`<dir>/mcp.json`）
pub fn get_kimi_code_mcp_path() -> PathBuf {
    crate::settings::get_kimi_code_dir().join("mcp.json")
}

fn kimi_code_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ============================================================================
// Type Definitions
// ============================================================================

/// Kimi Code 写入结果
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KimiCodeWriteOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
}

// ============================================================================
// Backup & Cleanup
// ============================================================================

/// 备份策略对齐 dsh_config：时间戳命名、同秒冲突追加计数后缀、写后备份清理。
/// 文件为 `<cc-switch 配置目录>/backups/kimi-code/kimi_code_{kind}_*.toml`，
/// `kind` 区分 config / mcp，清理按 kind 分别计数。
pub(crate) fn create_kimi_code_backup(kind: &str, source: &str) -> Result<PathBuf, AppError> {
    let backup_dir = get_app_config_dir().join("backups").join("kimi-code");
    fs::create_dir_all(&backup_dir).map_err(|e| AppError::io(&backup_dir, e))?;

    let base_id = format!("kimi_code_{kind}_{}", Local::now().format("%Y%m%d_%H%M%S"));
    let mut filename = format!("{base_id}.toml");
    let mut backup_path = backup_dir.join(&filename);
    let mut counter = 1;

    while backup_path.exists() {
        filename = format!("{base_id}_{counter}.toml");
        backup_path = backup_dir.join(&filename);
        counter += 1;
    }

    atomic_write(&backup_path, source.as_bytes())?;
    cleanup_kimi_code_backups(&backup_dir, kind)?;
    Ok(backup_path)
}

fn cleanup_kimi_code_backups(dir: &Path, kind: &str) -> Result<(), AppError> {
    let retain = effective_backup_retain_count();
    let prefix = format!("kimi_code_{kind}_");
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
                "Failed to remove old Kimi Code config backup {}: {err}",
                entry.path().display()
            );
        }
    }

    Ok(())
}

// ============================================================================
// Core TOML Read/Write
// ============================================================================

/// 读取 config.toml 为 toml_edit Document（保留注释与格式）。
/// 文件不存在/为空时返回空文档。
pub fn read_kimi_code_config() -> Result<DocumentMut, AppError> {
    let path = get_kimi_code_config_path();
    if !path.exists() {
        return Ok(DocumentMut::new());
    }

    let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    if content.trim().is_empty() {
        return Ok(DocumentMut::new());
    }

    content.parse::<DocumentMut>().map_err(|e| {
        AppError::Config(format!(
            "Failed to parse Kimi Code config.toml as TOML: {e}"
        ))
    })
}

/// 写入 config.toml（写锁 + 写前备份 + 原子写）。
pub fn write_kimi_code_config(doc: &DocumentMut) -> Result<KimiCodeWriteOutcome, AppError> {
    let _guard = kimi_code_write_lock().lock()?;
    write_kimi_code_config_locked(doc)
}

fn write_kimi_code_config_locked(doc: &DocumentMut) -> Result<KimiCodeWriteOutcome, AppError> {
    let path = get_kimi_code_config_path();

    let backup_path = if path.exists() {
        let existing = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
        if existing == doc.to_string() {
            None
        } else {
            Some(create_kimi_code_backup("config", &existing)?)
        }
    } else {
        None
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }

    atomic_write(&path, doc.to_string().as_bytes())?;

    log::debug!("Kimi Code config.toml written to {:?}", path);
    Ok(KimiCodeWriteOutcome {
        backup_path: backup_path.map(|p| p.display().to_string()),
    })
}

/// 读取 config.toml 并整体转为 JSON（服务层 read_live_settings 用）。
pub fn read_kimi_code_config_json() -> Result<serde_json::Value, AppError> {
    let path = get_kimi_code_config_path();
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    if content.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    let toml_value: toml::Value = toml::from_str(&content).map_err(|e| {
        AppError::Config(format!(
            "Failed to parse Kimi Code config.toml as TOML: {e}"
        ))
    })?;
    serde_json::to_value(toml_value)
        .map_err(|e| AppError::Config(format!("Failed to convert config.toml to JSON: {e}")))
}

/// mcp.json 的读-改-写入口（供 `mcp::kimi_code` 复用）：
/// 读取为 `serde_json::Value`（缺失/空 → 空对象），在写锁内执行 updater，
/// 写前备份 + 原子写。
pub(crate) fn update_kimi_code_mcp_json(
    updater: impl FnOnce(&mut serde_json::Value) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let _guard = kimi_code_write_lock().lock()?;
    let path = get_kimi_code_mcp_path();

    let mut root = if path.exists() {
        let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
        if content.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&content)
                .map_err(|e| AppError::Config(format!("Failed to parse Kimi Code mcp.json: {e}")))?
        }
    } else {
        serde_json::json!({})
    };

    updater(&mut root)?;

    let serialized =
        serde_json::to_string_pretty(&root).map_err(|e| AppError::Config(e.to_string()))?;

    if path.exists() {
        let existing = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
        if existing == serialized {
            return Ok(());
        }
        // mcp.json 是 JSON，备份仍走统一通道（扩展名不影响内容）
        create_kimi_code_backup("mcp", &existing)?;
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }
    atomic_write(&path, serialized.as_bytes())?;
    Ok(())
}

// ============================================================================
// Provider Management
// ============================================================================

/// cc-switch 管理的 provider 表名前缀（`providers."ccs-<id>"`）
const PROVIDER_PREFIX: &str = "ccs-";
/// cc-switch 管理的模型别名前缀（`models."ccs/<id>"`）
const MODEL_PREFIX: &str = "ccs/";

const ALLOWED_PROVIDER_TYPES: &[&str] = &[
    "kimi",
    "anthropic",
    "openai",
    "openai_responses",
    "google-genai",
    "vertexai",
];

fn provider_table_name(id: &str) -> String {
    format!("{PROVIDER_PREFIX}{id}")
}

fn model_alias(id: &str) -> String {
    format!("{MODEL_PREFIX}{id}")
}

/// provider 表名还原为 cc-switch provider id（非 ccs- 前缀返回 None）
fn provider_id_from_table(name: &str) -> Option<&str> {
    name.strip_prefix(PROVIDER_PREFIX)
}

/// 模型别名还原为 cc-switch provider id（非 ccs/ 前缀返回 None）
fn provider_id_from_alias(alias: &str) -> Option<&str> {
    alias.strip_prefix(MODEL_PREFIX)
}

fn providers_table(doc: &DocumentMut) -> Option<&Table> {
    doc.get("providers").and_then(Item::as_table)
}

fn models_table(doc: &DocumentMut) -> Option<&Table> {
    doc.get("models").and_then(Item::as_table)
}

fn providers_table_mut(doc: &mut DocumentMut) -> &mut Table {
    // toml_edit 的索引访问会按需创建隐式父表
    let item = doc["providers"].or_insert(toml_edit::table());
    item.as_table_mut().expect("providers 由本函数创建")
}

fn models_table_mut(doc: &mut DocumentMut) -> &mut Table {
    let item = doc["models"].or_insert(toml_edit::table());
    item.as_table_mut().expect("models 由本函数创建")
}

fn table_to_json(table: &Table) -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
    let doc = {
        let mut d = DocumentMut::new();
        d["x"] = Item::Table(table.clone());
        d
    };
    let toml_value: toml::Value = toml::from_str(&doc.to_string())
        .map_err(|e| AppError::Config(format!("TOML table 转换失败: {e}")))?;
    let json = serde_json::to_value(toml_value)
        .map_err(|e| AppError::Config(format!("TOML→JSON 转换失败: {e}")))?;
    Ok(json
        .get("x")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default())
}

/// 读取全部 cc-switch 管理的 provider（扁平 settings_config 形态）
///
/// 只返回 `ccs-` 前缀的条目；每个 provider 条目与同名模型别名
/// （`models."ccs/<id>"`）合并为一份扁平 JSON：`type`/`base_url`/`api_key`/
/// `custom_headers` 来自 provider 表，`model`/`max_context_size`/`capabilities`/
/// `support_efforts`/`default_effort`/`display_name` 来自模型表。
pub fn get_providers() -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
    let doc = read_kimi_code_config()?;
    let mut result = serde_json::Map::new();

    let Some(providers) = providers_table(&doc) else {
        return Ok(result);
    };

    for (table_name, item) in providers.iter() {
        let Some(id) = provider_id_from_table(table_name) else {
            continue;
        };
        let Some(table) = item.as_table() else {
            continue;
        };

        let mut config = table_to_json(table)?;

        // 合并模型别名字段（模型表值覆盖同名 provider 键的情况不会发生——
        // 两侧键集合不重叠——但 display_name 以模型表为准）
        if let Some(alias_item) = models_table(&doc).and_then(|models| models.get(&model_alias(id)))
        {
            if let Some(model_table) = alias_item.as_table() {
                let model_json = table_to_json(model_table)?;
                for (key, value) in model_json {
                    if key == "provider" {
                        continue; // 回链键不导出
                    }
                    config.insert(key, value);
                }
            }
        }

        result.insert(id.to_string(), serde_json::Value::Object(config));
    }

    Ok(result)
}

/// 读取单个 provider（不存在返回 Ok(None)）
pub fn get_provider(id: &str) -> Result<Option<serde_json::Value>, AppError> {
    Ok(get_providers()?.remove(id))
}

/// 用盘上已有条目填充新表的缺口（forward-compat：保留用户手加的未知键，
/// 顺带保留这些键上的注释/格式）
fn merge_preserve_unknown(new_table: &mut Table, existing: &Table) {
    for (key, item) in existing.iter() {
        if !new_table.contains_key(key) {
            new_table.insert(key, item.clone());
        }
    }
}

/// Upsert provider：写 `[providers."ccs-<id>"]` 与 `[models."ccs/<id>"]`。
///
/// settings_config（扁平契约，snake_case 对齐 config.toml 原生键名）：
/// `type`/`base_url`/`api_key`/`custom_headers` 进 provider 表；
/// `model`/`max_context_size`/`capabilities`/`support_efforts`/`default_effort`/
/// `display_name` 进模型表。未提供的可选键不写；盘上已有条目的未知键保留。
pub fn set_provider(
    id: &str,
    provider_config: serde_json::Value,
) -> Result<KimiCodeWriteOutcome, AppError> {
    validate_kimi_code_provider_key(id)?;
    let obj = provider_config.as_object().ok_or_else(|| {
        AppError::localized(
            "provider.kimi_code.settings.not_object",
            "Kimi Code 配置必须是 JSON 对象",
            "Kimi Code configuration must be a JSON object",
        )
    })?;

    let _guard = kimi_code_write_lock().lock()?;
    let mut doc = read_kimi_code_config()?;

    // ---- provider 表 ----
    let mut provider_table = Table::new();
    for key in ["type", "base_url", "api_key"] {
        if let Some(value) = obj.get(key).and_then(|v| v.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                provider_table.insert(key, toml_edit::value(trimmed));
            }
        }
    }
    if let Some(headers) = obj.get("custom_headers").and_then(|v| v.as_object()) {
        let mut headers_table = Table::new();
        for (hk, hv) in headers {
            if let Some(hv_str) = hv.as_str() {
                headers_table.insert(hk, toml_edit::value(hv_str));
            }
        }
        if !headers_table.is_empty() {
            provider_table.insert("custom_headers", Item::Table(headers_table));
        }
    }

    // ---- 模型表 ----
    let mut model_table = Table::new();
    model_table.insert(
        "provider",
        toml_edit::value(provider_table_name(id).as_str()),
    );
    if let Some(model) = obj.get("model").and_then(|v| v.as_str()) {
        let trimmed = model.trim();
        if !trimmed.is_empty() {
            model_table.insert("model", toml_edit::value(trimmed));
        }
    }
    if let Some(ctx) = obj.get("max_context_size").and_then(|v| v.as_i64()) {
        model_table.insert("max_context_size", toml_edit::value(ctx));
    }
    if let Some(name) = obj.get("display_name").and_then(|v| v.as_str()) {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            model_table.insert("display_name", toml_edit::value(trimmed));
        }
    }
    if let Some(effort) = obj.get("default_effort").and_then(|v| v.as_str()) {
        let trimmed = effort.trim();
        if !trimmed.is_empty() {
            model_table.insert("default_effort", toml_edit::value(trimmed));
        }
    }
    for key in ["capabilities", "support_efforts"] {
        if let Some(values) = obj.get(key).and_then(|v| v.as_array()) {
            let mut array = toml_edit::Array::new();
            for value in values {
                if let Some(s) = value.as_str() {
                    array.push(s);
                }
            }
            if !array.is_empty() {
                model_table.insert(key, toml_edit::value(array));
            }
        }
    }

    // forward-compat：保留盘上同名单元的未知键
    if let Some(existing) = providers_table(&doc)
        .and_then(|t| t.get(&provider_table_name(id)))
        .and_then(Item::as_table)
    {
        merge_preserve_unknown(&mut provider_table, existing);
    }
    if let Some(existing) = models_table(&doc)
        .and_then(|t| t.get(&model_alias(id)))
        .and_then(Item::as_table)
    {
        merge_preserve_unknown(&mut model_table, existing);
    }

    providers_table_mut(&mut doc).insert(&provider_table_name(id), Item::Table(provider_table));
    models_table_mut(&mut doc).insert(&model_alias(id), Item::Table(model_table));

    write_kimi_code_config_locked(&doc)
}

/// 删除 provider 及其模型别名；若 `default_model` 指向该别名则一并清除
/// （Kimi Code 对悬空 default_model 启动即报错，不能留）。
pub fn remove_provider(id: &str) -> Result<KimiCodeWriteOutcome, AppError> {
    let _guard = kimi_code_write_lock().lock()?;
    let mut doc = read_kimi_code_config()?;

    let provider_name = provider_table_name(id);
    let alias = model_alias(id);

    if let Some(table) = doc.get_mut("providers").and_then(Item::as_table_mut) {
        table.remove(&provider_name);
    }
    if let Some(table) = doc.get_mut("models").and_then(Item::as_table_mut) {
        table.remove(&alias);
    }
    if doc.get("default_model").and_then(Item::as_str) == Some(alias.as_str()) {
        doc.as_table_mut().remove("default_model");
    }

    write_kimi_code_config_locked(&doc)
}

// ============================================================================
// Default Model（当前供应商/模型）
// ============================================================================

/// 读取顶层 `default_model`（不存在返回 Ok(None)）
pub fn get_default_model() -> Result<Option<String>, AppError> {
    let doc = read_kimi_code_config()?;
    Ok(doc
        .get("default_model")
        .and_then(Item::as_str)
        .map(|s| s.to_string()))
}

/// 当前 default_model 对应的 cc-switch provider id（非 ccs/ 别名 → None）
pub fn default_model_provider_id() -> Result<Option<String>, AppError> {
    Ok(get_default_model()?
        .as_deref()
        .and_then(provider_id_from_alias)
        .map(str::to_string))
}

/// 切换当前模型：`default_model = "ccs/<id>"`。
/// 模型别名不存在时拒绝写入（Kimi Code 对悬空 default_model 启动即报错）。
pub fn set_default_model(id: &str) -> Result<KimiCodeWriteOutcome, AppError> {
    let alias = model_alias(id);

    let _guard = kimi_code_write_lock().lock()?;
    let mut doc = read_kimi_code_config()?;

    let alias_exists = models_table(&doc).is_some_and(|t| t.contains_key(&alias));
    if !alias_exists {
        return Err(AppError::localized(
            "kimi_code.default_model.missing_alias",
            format!("Kimi Code 模型别名不存在: {alias}（请先写入供应商）"),
            format!("Kimi Code model alias does not exist: {alias} (write the provider first)"),
        ));
    }

    doc["default_model"] = toml_edit::value(alias.as_str());
    write_kimi_code_config_locked(&doc)
}

// ============================================================================
// Validation
// ============================================================================

/// provider key 校验：只允许 `[a-zA-Z0-9_-]`（要进 TOML 表名与模型别名）
pub fn validate_kimi_code_provider_key(key: &str) -> Result<(), AppError> {
    let valid = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(AppError::localized(
            "provider.kimi_code.key.invalid",
            format!("Kimi Code 供应商标识 '{key}' 非法：只允许字母、数字、下划线和连字符"),
            format!(
                "Invalid Kimi Code provider key '{key}': only letters, digits, '_' and '-' are allowed"
            ),
        ))
    }
}

/// 校验扁平 settings_config：`type` 六枚举、`base_url` 非空、`model` 非空。
pub fn validate_kimi_code_provider_config(config: &serde_json::Value) -> Result<(), AppError> {
    let obj = config.as_object().ok_or_else(|| {
        AppError::localized(
            "provider.kimi_code.settings.not_object",
            "Kimi Code 配置必须是 JSON 对象",
            "Kimi Code configuration must be a JSON object",
        )
    })?;

    let provider_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::localized(
                "provider.kimi_code.type.missing",
                "Kimi Code 供应商缺少 type",
                "Kimi Code provider is missing `type`",
            )
        })?;
    if !ALLOWED_PROVIDER_TYPES.contains(&provider_type) {
        return Err(AppError::localized(
            "provider.kimi_code.type.invalid",
            format!(
                "Kimi Code 供应商 type '{provider_type}' 非法；允许值: {}",
                ALLOWED_PROVIDER_TYPES.join(", ")
            ),
            format!(
                "Invalid Kimi Code provider type '{provider_type}'; allowed: {}",
                ALLOWED_PROVIDER_TYPES.join(", ")
            ),
        ));
    }

    let base_url = obj
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if base_url.is_empty() {
        return Err(AppError::localized(
            "provider.kimi_code.base_url.missing",
            "Kimi Code 供应商缺少 base_url",
            "Kimi Code provider is missing `base_url`",
        ));
    }

    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if model.is_empty() {
        return Err(AppError::localized(
            "provider.kimi_code.model.missing",
            "Kimi Code 供应商缺少 model",
            "Kimi Code provider is missing `model`",
        ));
    }

    Ok(())
}

// ============================================================================
// Common Config Snippet（通用配置片段，TOML 文本，合并进 config.toml 顶层）
// ============================================================================

/// 通用配置不允许触碰的顶层键（cc-switch 自行管理的区域）
const PROTECTED_TOP_LEVEL_KEYS: &[&str] =
    &["providers", "models", "default_model", "secondary_model"];

fn is_protected_top_level_key(key: &str) -> bool {
    PROTECTED_TOP_LEVEL_KEYS.contains(&key)
}

/// 解析 TOML 片段（允许空/纯注释，返回空文档）
pub(crate) fn parse_common_config_snippet(snippet: &str) -> Result<DocumentMut, AppError> {
    let trimmed = snippet.trim();
    if trimmed.is_empty() {
        return Ok(DocumentMut::new());
    }
    trimmed.parse::<DocumentMut>().map_err(|e| {
        AppError::localized(
            "kimi_code_common_config_invalid",
            format!("无效的 Kimi Code 通用配置 TOML: {e}"),
            format!("Invalid Kimi Code common config TOML: {e}"),
        )
    })
}

/// TOML 深合并：表递归合并，其余值整体替换
fn deep_merge_toml(target: &mut Item, source: &Item) {
    if let (Some(target_table), Some(source_table)) = (target.as_table_mut(), source.as_table()) {
        for (key, source_item) in source_table.iter() {
            match target_table.entry(key) {
                toml_edit::Entry::Occupied(mut entry) => {
                    deep_merge_toml(entry.get_mut(), source_item);
                }
                toml_edit::Entry::Vacant(entry) => {
                    entry.insert(source_item.clone());
                }
            }
        }
    } else {
        *target = source.clone();
    }
}

/// 应用通用配置片段：逐顶层键深合并进 config.toml；保护键跳过并告警
pub fn apply_kimi_code_common_config(snippet: &str) -> Result<(), AppError> {
    let snippet_doc = parse_common_config_snippet(snippet)?;
    if snippet_doc.is_empty() {
        return Ok(());
    }

    let _guard = kimi_code_write_lock().lock()?;
    let mut doc = read_kimi_code_config()?;

    for (key, item) in snippet_doc.iter() {
        if is_protected_top_level_key(key) {
            log::warn!("Kimi Code 通用配置跳过保护键: {key}");
            continue;
        }
        match doc.as_table_mut().entry(key) {
            toml_edit::Entry::Occupied(mut entry) => {
                deep_merge_toml(entry.get_mut(), item);
            }
            toml_edit::Entry::Vacant(entry) => {
                entry.insert(item.clone());
            }
        }
    }

    write_kimi_code_config_locked(&doc)?;
    Ok(())
}

/// 值相等（用 Display 文本比较；对本场景的标量/内联结构足够）
fn toml_item_equals(a: &Item, b: &Item) -> bool {
    a.to_string().trim() == b.to_string().trim()
}

/// 按键路径精确移除：仅当现有值与片段值相等时才删；掏空的可选子表回收
fn remove_matching_toml(target: &mut Table, source: &Table) {
    for (key, source_item) in source.iter() {
        let Some(target_item) = target.get_mut(key) else {
            continue;
        };
        match (target_item.as_table_mut(), source_item.as_table()) {
            (Some(target_table), Some(source_table)) => {
                remove_matching_toml(target_table, source_table);
                if target_table.is_empty() {
                    target.remove(key);
                }
            }
            _ => {
                if toml_item_equals(target_item, source_item) {
                    target.remove(key);
                }
            }
        }
    }
}

/// 移除通用配置片段（值已被用户改走的键不动；保护键不触碰）
pub fn remove_kimi_code_common_config(snippet: &str) -> Result<(), AppError> {
    let snippet_doc = parse_common_config_snippet(snippet)?;
    if snippet_doc.is_empty() {
        return Ok(());
    }

    let _guard = kimi_code_write_lock().lock()?;
    let mut doc = read_kimi_code_config()?;

    let mut filtered = Table::new();
    for (key, item) in snippet_doc.iter() {
        if is_protected_top_level_key(key) {
            continue;
        }
        filtered.insert(key, item.clone());
    }
    remove_matching_toml(doc.as_table_mut(), &filtered);

    write_kimi_code_config_locked(&doc)?;
    Ok(())
}

/// 判断片段内容是否已包含在 config.toml 顶层（保护键不参与判定）
fn toml_contains(target: &Item, source: &Item) -> bool {
    match (target.as_table(), source.as_table()) {
        (Some(target_table), Some(source_table)) => source_table.iter().all(|(key, item)| {
            target_table
                .get(key)
                .is_some_and(|target_item| toml_contains(target_item, item))
        }),
        _ => toml_item_equals(target, source),
    }
}

pub fn kimi_code_common_config_applied(snippet: &str) -> bool {
    let Ok(snippet_doc) = parse_common_config_snippet(snippet) else {
        return false;
    };
    if snippet_doc.is_empty() {
        return false;
    }
    let Ok(doc) = read_kimi_code_config() else {
        return false;
    };

    let applied = snippet_doc.iter().all(|(key, item)| {
        if is_protected_top_level_key(key) {
            return true;
        }
        doc.get(key)
            .is_some_and(|target_item| toml_contains(target_item, item))
    });
    applied
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

    /// 隔离临时 HOME：保存/恢复 CC_SWITCH_TEST_HOME，并中和 KIMI_CODE_HOME
    /// （防止真实 Kimi Code 安装把测试带出临时目录）。
    fn with_test_home<T>(test_fn: impl FnOnce() -> T) -> T {
        let _guard = test_guard();
        let tmp = tempfile::tempdir().unwrap();
        let old_test_home = std::env::var_os("CC_SWITCH_TEST_HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", tmp.path());
        let old_kimi_home = std::env::var_os("KIMI_CODE_HOME");
        std::env::remove_var("KIMI_CODE_HOME");
        let result = test_fn();
        match old_kimi_home {
            Some(value) => std::env::set_var("KIMI_CODE_HOME", value),
            None => std::env::remove_var("KIMI_CODE_HOME"),
        }
        match old_test_home {
            Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
        result
    }

    fn sample_config() -> serde_json::Value {
        json!({
            "type": "anthropic",
            "base_url": "https://api.example.com",
            "api_key": "sk-test",
            "model": "claude-sonnet-5",
            "max_context_size": 1000000,
            "display_name": "Demo Relay"
        })
    }

    #[test]
    #[serial]
    fn provider_roundtrip_and_remove_clears_dangling_default() {
        with_test_home(|| {
            set_provider("demo", sample_config()).expect("set_provider");

            let providers = get_providers().expect("get_providers");
            let entry = providers.get("demo").expect("demo provider");
            assert_eq!(
                entry.get("type").and_then(|v| v.as_str()),
                Some("anthropic")
            );
            assert_eq!(
                entry.get("base_url").and_then(|v| v.as_str()),
                Some("https://api.example.com")
            );
            assert_eq!(
                entry.get("api_key").and_then(|v| v.as_str()),
                Some("sk-test")
            );
            assert_eq!(
                entry.get("model").and_then(|v| v.as_str()),
                Some("claude-sonnet-5")
            );
            assert_eq!(
                entry.get("max_context_size").and_then(|v| v.as_i64()),
                Some(1000000)
            );
            assert_eq!(
                entry.get("display_name").and_then(|v| v.as_str()),
                Some("Demo Relay")
            );

            // 盘上形态：provider/model 两张表
            let raw = fs::read_to_string(get_kimi_code_config_path()).unwrap();
            assert!(
                raw.contains("[providers.ccs-demo]") || raw.contains("[providers.\"ccs-demo\"]")
            );
            assert!(raw.contains("ccs/demo"));

            set_default_model("demo").expect("set_default_model");
            assert_eq!(get_default_model().unwrap().as_deref(), Some("ccs/demo"));
            assert_eq!(
                default_model_provider_id().unwrap().as_deref(),
                Some("demo")
            );

            remove_provider("demo").expect("remove_provider");
            assert!(get_providers().unwrap().get("demo").is_none());
            // 悬空 default_model 已清除
            assert_eq!(get_default_model().unwrap(), None);
        });
    }

    #[test]
    #[serial]
    fn preserves_user_entries_and_comments() {
        with_test_home(|| {
            let path = get_kimi_code_config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                "# 用户注释\ntelemetry = false\n\n[providers.\"managed:kimi-code\"]\ntype = \"kimi\"\n",
            )
            .unwrap();

            set_provider("demo", sample_config()).unwrap();

            let raw = fs::read_to_string(&path).unwrap();
            assert!(raw.contains("# 用户注释"));
            assert!(raw.contains("telemetry = false"));
            assert!(raw.contains("managed:kimi-code"));
        });
    }

    #[test]
    #[serial]
    fn set_default_model_rejects_missing_alias() {
        with_test_home(|| {
            let err = set_default_model("ghost").unwrap_err();
            assert!(err.to_string().contains("ccs/ghost"));
        });
    }

    #[test]
    fn validate_accepts_valid_config() {
        validate_kimi_code_provider_config(&sample_config()).unwrap();
    }

    #[test]
    fn validate_rejects_bad_type_and_missing_fields() {
        assert!(validate_kimi_code_provider_config(&json!({
            "type": "responses",
            "base_url": "https://x",
            "model": "m"
        }))
        .unwrap_err()
        .to_string()
        .contains("type"));
        assert!(validate_kimi_code_provider_config(&json!({
            "type": "anthropic",
            "model": "m"
        }))
        .unwrap_err()
        .to_string()
        .contains("base_url"));
        assert!(validate_kimi_code_provider_config(&json!({
            "type": "anthropic",
            "base_url": "https://x"
        }))
        .unwrap_err()
        .to_string()
        .contains("model"));
    }

    #[test]
    fn validate_key_charset() {
        validate_kimi_code_provider_key("ok-key_1").unwrap();
        assert!(validate_kimi_code_provider_key("bad key").is_err());
        assert!(validate_kimi_code_provider_key("bad.key").is_err());
        assert!(validate_kimi_code_provider_key("").is_err());
    }

    #[test]
    #[serial]
    fn common_config_apply_applied_remove_roundtrip() {
        with_test_home(|| {
            let snippet = "[thinking]\nenabled = true\neffort = \"high\"\n";
            assert!(!kimi_code_common_config_applied(snippet));

            apply_kimi_code_common_config(snippet).unwrap();
            assert!(kimi_code_common_config_applied(snippet));

            let raw = fs::read_to_string(get_kimi_code_config_path()).unwrap();
            assert!(raw.contains("[thinking]"));

            remove_kimi_code_common_config(snippet).unwrap();
            assert!(!kimi_code_common_config_applied(snippet));
            let raw = fs::read_to_string(get_kimi_code_config_path()).unwrap();
            assert!(!raw.contains("[thinking]"));
        });
    }

    #[test]
    #[serial]
    fn common_config_skips_protected_keys() {
        with_test_home(|| {
            let snippet = "default_model = \"ccs/evil\"\ntelemetry = false\n";
            apply_kimi_code_common_config(snippet).unwrap();
            let doc = read_kimi_code_config().unwrap();
            assert!(doc.get("default_model").is_none());
            assert_eq!(doc.get("telemetry").and_then(Item::as_bool), Some(false));
        });
    }

    #[test]
    #[serial]
    fn common_config_remove_keeps_user_modified_values() {
        with_test_home(|| {
            let snippet = "telemetry = false\n";
            apply_kimi_code_common_config(snippet).unwrap();
            // 用户改走了值
            let mut doc = read_kimi_code_config().unwrap();
            doc["telemetry"] = toml_edit::value(true);
            write_kimi_code_config(&doc).unwrap();
            // 值不等则不删
            remove_kimi_code_common_config(snippet).unwrap();
            let doc = read_kimi_code_config().unwrap();
            assert_eq!(doc.get("telemetry").and_then(Item::as_bool), Some(true));
        });
    }

    #[test]
    #[serial]
    fn write_creates_backup_on_overwrite() {
        with_test_home(|| {
            set_provider("demo", sample_config()).unwrap();
            let mut changed = sample_config();
            changed["api_key"] = json!("sk-changed");
            let outcome = set_provider("demo", changed).unwrap();
            assert!(outcome.backup_path.is_some());
            let backup = outcome.backup_path.unwrap();
            assert!(backup.contains("kimi_code_config_"));
            assert!(std::path::Path::new(&backup).exists());
        });
    }
}
