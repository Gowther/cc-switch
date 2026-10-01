//! zcode（智谱 ZCode，z.ai 的桌面 Agentic IDE）配置文件读写模块
//!
//! 处理 `<zcode_dir>`（默认 `~/.zcode`，解析见 `crate::settings::get_zcode_dir`；
//! zcode 没有环境变量覆盖层）下两个 JSON 文件的读写。ZCode 对外部写入的
//! provider 配置需重启 ZCode 才生效（官方文档明确）。
//!
//! ## `v2/config.json`（provider 配置）
//!
//! ```json
//! {
//!   "provider": {
//!     "<providerId>": {
//!       "name": "显示名",
//!       "kind": "openai-compatible",
//!       "source": "custom",
//!       "options": { "apiKey": "sk-...", "baseURL": "https://...", "apiKeyRequired": true },
//!       "models": { "<modelId>": { "name": "...", "reasoning": { ... }, "limit": { ... } } }
//!     }
//!   }
//! }
//! ```
//!
//! - `kind` 只允许 `anthropic` / `openai` / `openai-compatible`；写非法值会让
//!   ZCode safeParse 失败并**清空整个 provider 配置**——这是最大的坑，因此
//!   `validate_zcode_provider_config` 把 kind 校验放在首位，`set_provider`
//!   内部也做了同样的防御性拦截。
//! - provider id 字符集 `[a-zA-Z0-9_:-]+`（`validate_zcode_provider_key`）。
//! - provider/model 级未知字段（如 `zcode.modified`）整体保留
//!   （forward-compat merge，对齐 dsh）；文件其他顶层键原样保留。
//! - zcode 没有"当前激活 provider"的概念（模型选择是 ZCode GUI 内部状态），
//!   因此本模块没有 default-model 钩子（与 dsh 的 `agent-default-model` 不同）。
//!
//! ## `cli/config.json`（MCP / Agent 配置）
//!
//! 本模块承担"通用配置片段"的深合并（保护键 `mcp` 不合并）与闭包式
//! 读-改-写入口（`update_zcode_cli_config`）；`mcp.servers` 的具体
//! 同步/导入逻辑在 `mcp::zcode` 层。
//!
//! ## 与 cc-switch `Provider.settings_config`（JSON）的契约
//!
//! settings_config 是扁平形态：`{kind, baseURL, apiKey, apiKeyRequired?,
//! headers?, models?: [{id, name?, reasoning?, limit?}], displayName?}`。
//! 写 zcode 时：`displayName`→`name`，`apiKey/baseURL/apiKeyRequired/headers`
//! 收进 `options`，`models` 数组转 map（key=id），固定 `source: "custom"`；
//! 读回时反向摊平。apiKey 明文存 `options.apiKey`（zcode 原生如此，无凭据
//! 分离——与 dsh 的 `.credentials.yaml` 拆分不同）。
//!
//! 与 dsh_config 的差异：zcode 是纯 JSON（serde_json Value，`preserve_order`
//! 保持插入序、不重排用户文件里已有的键），无 YAML/凭据拆分/default-model
//! 钩子；备份策略、写锁、`atomic_write` 与保留数量
//! （`effective_backup_retain_count`）完全对齐。

use crate::config::{atomic_write, get_app_config_dir};
use crate::error::AppError;
use crate::settings::{effective_backup_retain_count, get_zcode_dir};
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// v2/config.json 中 provider 所在的顶层键
const PROVIDER_KEY: &str = "provider";

/// `options` 里已知的四个连接键（官方文档明确 options 只认这四个；
/// 其余键 ZCode 静默忽略、不写进请求体）
const KNOWN_OPTION_KEYS: &[&str] = &["apiKey", "baseURL", "apiKeyRequired", "headers"];

/// zcode `kind` 合法值集合（ZCode safeParse 只认这三个；写非法值会让
/// ZCode 解析失败并清空整个 provider 配置）
const ALLOWED_KINDS: &[&str] = &["anthropic", "openai", "openai-compatible"];

// ============================================================================
// Path Functions
// ============================================================================

/// 获取 zcode provider 配置路径（`<zcode_dir>/v2/config.json`）
pub fn get_zcode_settings_path() -> PathBuf {
    get_zcode_dir().join("v2").join("config.json")
}

/// 获取 zcode CLI/Agent 配置路径（`<zcode_dir>/cli/config.json`）。
/// `mcp.servers` 也在此文件（MCP 接线属后续任务，路径函数先行可被复用）。
pub fn get_zcode_cli_config_path() -> PathBuf {
    get_zcode_dir().join("cli").join("config.json")
}

fn zcode_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ============================================================================
// Type Definitions
// ============================================================================

/// zcode 写入结果
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ZcodeWriteOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
}

// ============================================================================
// Core JSON Read/Write
// ============================================================================

/// 读取 JSON 配置文件顶层 Object；文件不存在/为空时返回空 Object；
/// 解析失败或顶层非 Object 报 Config 错误（而不是覆盖，避免误毁用户数据）。
fn read_json_object(path: &Path) -> Result<Map<String, Value>, AppError> {
    if !path.exists() {
        return Ok(Map::new());
    }

    let content = fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    if content.trim().is_empty() {
        return Ok(Map::new());
    }

    let value: Value = serde_json::from_str(&content).map_err(|e| {
        AppError::Config(format!(
            "Failed to parse zcode config {} as JSON: {e}",
            path.display()
        ))
    })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(AppError::Config(format!(
            "zcode config {} top level must be a JSON object",
            path.display()
        ))),
    }
}

/// 读取 `v2/config.json` 顶层 Object（不存在/空 → 空 Object）
pub fn read_zcode_settings() -> Result<Map<String, Value>, AppError> {
    read_json_object(&get_zcode_settings_path())
}

/// 读取 `cli/config.json` 顶层 Object（不存在/空 → 空 Object）
pub fn read_zcode_cli_config() -> Result<Map<String, Value>, AppError> {
    read_json_object(&get_zcode_cli_config_path())
}

/// 序列化为 pretty JSON（2 空格缩进 + 尾换行，对齐仓库 write_json_file
/// 风格；`preserve_order` 保持插入序，不重排已有键）。
fn serialize_json_object(root: &Map<String, Value>) -> Result<String, AppError> {
    let mut serialized =
        serde_json::to_string_pretty(root).map_err(|e| AppError::JsonSerialize { source: e })?;
    serialized.push('\n');
    Ok(serialized)
}

/// Inner write helper — caller must already hold the write lock.
///
/// 内容与磁盘一致时 no-op（不备份、不写盘）；写前按 `backup_kind` 备份。
fn write_json_object_locked(
    path: &Path,
    backup_kind: &str,
    root: &Map<String, Value>,
) -> Result<ZcodeWriteOutcome, AppError> {
    let raw = if path.exists() {
        fs::read_to_string(path).map_err(|e| AppError::io(path, e))?
    } else {
        String::new()
    };

    let serialized = serialize_json_object(root)?;

    if serialized == raw {
        return Ok(ZcodeWriteOutcome::default());
    }

    let backup_path = if !raw.is_empty() {
        Some(create_zcode_backup(backup_kind, &raw)?)
    } else {
        None
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }

    atomic_write(path, serialized.as_bytes())?;

    log::debug!("zcode {backup_kind} config written to {:?}", path);
    Ok(ZcodeWriteOutcome {
        backup_path: backup_path.map(|p| p.display().to_string()),
    })
}

/// 写入 `v2/config.json`（写锁 + 写前备份 + atomic_write）
pub fn write_zcode_settings(root: &Map<String, Value>) -> Result<ZcodeWriteOutcome, AppError> {
    let _guard = zcode_write_lock().lock()?;
    write_json_object_locked(&get_zcode_settings_path(), "providers", root)
}

/// 写入 `cli/config.json`（写锁 + 写前备份 + atomic_write）
pub fn write_zcode_cli_config(root: &Map<String, Value>) -> Result<ZcodeWriteOutcome, AppError> {
    let _guard = zcode_write_lock().lock()?;
    write_json_object_locked(&get_zcode_cli_config_path(), "cli", root)
}

/// 在写锁内对 `cli/config.json` 做闭包式读-改-写（对齐 hermes 的
/// `update_mcp_servers_yaml`，避免 mcp 层与 common config 写入之间的
/// TOCTOU——两者共享同一文件与同一把锁）。
pub(crate) fn update_zcode_cli_config(
    f: impl FnOnce(&mut Map<String, Value>) -> Result<(), AppError>,
) -> Result<ZcodeWriteOutcome, AppError> {
    let _guard = zcode_write_lock().lock()?;
    let mut root = read_zcode_cli_config()?;
    f(&mut root)?;
    write_json_object_locked(&get_zcode_cli_config_path(), "cli", &root)
}

// ============================================================================
// Backup & Cleanup
// ============================================================================

/// 备份策略完全复用 dsh_config 的做法：时间戳命名、同秒冲突追加计数后缀、
/// 写后备份清理。文件为 `<cc-switch 配置目录>/backups/zcode/zcode_{kind}_*.json`，
/// `kind` 区分 providers（v2/config.json）/ cli（cli/config.json），
/// 清理按 kind 分别计数。
pub(crate) fn create_zcode_backup(kind: &str, source: &str) -> Result<PathBuf, AppError> {
    let backup_dir = get_app_config_dir().join("backups").join("zcode");
    fs::create_dir_all(&backup_dir).map_err(|e| AppError::io(&backup_dir, e))?;

    let base_id = format!("zcode_{kind}_{}", Local::now().format("%Y%m%d_%H%M%S"));
    let mut filename = format!("{base_id}.json");
    let mut backup_path = backup_dir.join(&filename);
    let mut counter = 1;

    while backup_path.exists() {
        filename = format!("{base_id}_{counter}.json");
        backup_path = backup_dir.join(&filename);
        counter += 1;
    }

    atomic_write(&backup_path, source.as_bytes())?;
    cleanup_zcode_backups(&backup_dir, kind)?;
    Ok(backup_path)
}

fn cleanup_zcode_backups(dir: &Path, kind: &str) -> Result<(), AppError> {
    let retain = effective_backup_retain_count();
    let prefix = format!("zcode_{kind}_");
    let mut entries = fs::read_dir(dir)
        .map_err(|e| AppError::io(dir, e))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_name().to_string_lossy().starts_with(&prefix)
                && entry
                    .path()
                    .extension()
                    .map(|ext| ext == "json")
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
                "Failed to remove old zcode config backup {}: {err}",
                entry.path().display()
            );
        }
    }

    Ok(())
}

// ============================================================================
// JSON Value Helpers
// ============================================================================

/// 取 `root[key]` 的可变 Object；缺失时新建，存在但不是 Object
/// （损坏/旧格式）时告警并重建为空 Object。
pub(crate) fn ensure_child_object<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
) -> &'a mut Map<String, Value> {
    let needs_reset = match root.get(key) {
        Some(value) => !value.is_object(),
        None => true,
    };
    if needs_reset {
        if root.contains_key(key) {
            log::warn!("zcode config: '{key}' is not an object, resetting");
        }
        root.insert(key.to_string(), Value::Object(Map::new()));
    }
    match root.get_mut(key) {
        Some(Value::Object(map)) => map,
        _ => unreachable!("just inserted an object"),
    }
}

// ============================================================================
// Provider Functions
// ============================================================================

/// `models` map → 数组（key 作为元素 `id`；元素其余字段原样保留）。
/// 盘上不是 map 时原样透传（防御，ZCode 原生格式是 map）。
fn models_map_to_array(value: &Value) -> Value {
    let Some(map) = value.as_object() else {
        return value.clone();
    };
    let mut arr = Vec::with_capacity(map.len());
    for (id, cfg) in map {
        let mut item = match cfg.as_object() {
            Some(obj) => obj.clone(),
            None => Map::new(),
        };
        item.insert("id".to_string(), Value::String(id.clone()));
        arr.push(Value::Object(item));
    }
    Value::Array(arr)
}

/// settings_config 的 `models` 数组 → zcode 原生 map（key = 元素 `id`）。
/// 元素缺非空 `id` 时跳过并告警（service 层的 validate 会先拦截）。
/// 不是数组时原样透传（防御）。
fn models_array_to_map(value: &Value) -> Value {
    let Some(arr) = value.as_array() else {
        return value.clone();
    };
    let mut map = Map::new();
    for item in arr {
        let Some(obj) = item.as_object() else {
            log::warn!("zcode models[] element is not an object, skipped");
            continue;
        };
        let Some(id) = obj
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            log::warn!("zcode models[] element missing non-empty id, skipped");
            continue;
        };
        let mut entry = obj.clone();
        entry.remove("id");
        map.insert(id.to_string(), Value::Object(entry));
    }
    Value::Object(map)
}

/// zcode 原生 provider 条目 → cc-switch 扁平 settings_config：
/// `name`→`displayName`，`options.{apiKey,baseURL,apiKeyRequired,headers}` 提升到
/// 顶层，`models` map 转数组（key 作为元素 `id`），`source` 不导出（写回时固定
/// "custom"），其余 provider 级未知字段原样透传。
fn flatten_provider_entry(entry: &Map<String, Value>) -> Value {
    let mut flat = Map::new();

    for (key, value) in entry {
        match key.as_str() {
            "name" => {
                if let Some(name) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    flat.insert("displayName".to_string(), Value::String(name.to_string()));
                }
            }
            "options" => {
                if let Some(options) = value.as_object() {
                    for opt_key in KNOWN_OPTION_KEYS {
                        if let Some(v) = options.get(*opt_key) {
                            flat.insert(opt_key.to_string(), v.clone());
                        }
                    }
                }
            }
            "models" => {
                flat.insert("models".to_string(), models_map_to_array(value));
            }
            // zcode 内部记账字段，写回时固定 "custom"，不导出到 settings_config
            "source" => {}
            _ => {
                flat.insert(key.clone(), value.clone());
            }
        }
    }

    Value::Object(flat)
}

/// 获取全部供应商（`provider` map），每项摊平为扁平 settings_config 形态。
pub fn get_providers() -> Result<Map<String, Value>, AppError> {
    let root = read_zcode_settings()?;
    let mut map = Map::new();

    let Some(providers) = root.get(PROVIDER_KEY).and_then(|v| v.as_object()) else {
        return Ok(map);
    };

    for (key, value) in providers {
        let key_str = key.trim();
        if key_str.is_empty() {
            continue;
        }
        let Some(entry) = value.as_object() else {
            log::debug!("Skipping zcode providers['{key_str}']: not an object");
            continue;
        };
        map.insert(key_str.to_string(), flatten_provider_entry(entry));
    }

    Ok(map)
}

/// 获取单个供应商（扁平 settings_config 形态）
pub fn get_provider(key: &str) -> Result<Option<Value>, AppError> {
    Ok(get_providers()?.get(key).cloned())
}

/// settings_config（扁平契约 JSON 对象）→ zcode 原生 provider 条目。
///
/// 注意：`name` 仅在 payload 携带非空 `displayName` 时写入；否则留给
/// forward-compat merge 从盘上填充（新建时回退为 provider key）。
fn build_provider_entry(obj: &Map<String, Value>) -> Map<String, Value> {
    let mut entry = Map::new();

    if let Some(name) = obj
        .get("displayName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        entry.insert("name".to_string(), Value::String(name.to_string()));
    }

    if let Some(kind) = obj.get("kind") {
        entry.insert("kind".to_string(), kind.clone());
    }

    entry.insert("source".to_string(), Value::String("custom".to_string()));

    // options 只写四个已知键
    let mut options = Map::new();
    for opt_key in KNOWN_OPTION_KEYS {
        if let Some(v) = obj.get(*opt_key) {
            options.insert(opt_key.to_string(), v.clone());
        }
    }
    if !options.is_empty() {
        entry.insert("options".to_string(), Value::Object(options));
    }

    if let Some(models) = obj.get("models") {
        entry.insert("models".to_string(), models_array_to_map(models));
    }

    // 其余键透传到 provider 级（如 `zcode.modified` 元数据；若调用方传入的
    // 是 zcode 原生嵌套形态，`options` 也会经此原样透传，合并时仍按 options
    // 逐键填充缺口）
    for (k, v) in obj {
        let known = matches!(
            k.as_str(),
            "displayName"
                | "kind"
                | "source"
                | "models"
                | "apiKey"
                | "baseURL"
                | "apiKeyRequired"
                | "headers"
        );
        if !known {
            entry.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }

    entry
}

/// Forward-compat merge：existing（盘上）填充 entry（本次 payload）的缺口。
/// `options` 内部逐键填充（保留盘上 options 里的未知键）；`models` 若 payload
/// 显式携带则整体替换，否则沿用盘上；其余 provider 级字段按键填充。
fn merge_preserve_unknown(entry: &mut Map<String, Value>, existing: &Map<String, Value>) {
    for (k, v) in existing {
        if k == "options" {
            if let (Some(new_opts), Some(old_opts)) = (
                entry.get_mut("options").and_then(|v| v.as_object_mut()),
                v.as_object(),
            ) {
                for (ok, ov) in old_opts {
                    new_opts.entry(ok.clone()).or_insert_with(|| ov.clone());
                }
                continue;
            }
        }
        entry.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

/// Upsert `provider.<key>`。
///
/// settings_config（扁平契约）→ zcode 原生条目（`displayName`→`name`、
/// 四个连接键收进 `options`、`models` 数组转 map、固定 `source: "custom"`、
/// 未知键透传 provider 级），并保留盘上该 provider 的未知字段（对齐 dsh 的
/// forward-compat merge）；文件其他顶层键原样不动。
///
/// provider key 与（若携带的）`kind` 在写盘前过校验——非法值会让 ZCode
/// 解析失败并清空整个 provider 配置，必须拦截。整个读-改-写在写锁内完成，
/// 避免 TOCTOU。
pub fn set_provider(key: &str, provider_config: Value) -> Result<ZcodeWriteOutcome, AppError> {
    validate_zcode_provider_key(key)?;

    let obj = provider_config.as_object().ok_or_else(|| {
        AppError::localized(
            "provider.zcode.configNotObject",
            "zcode 供应商配置必须是 JSON 对象",
            "zcode provider config must be a JSON object.",
        )
    })?;

    // 防御性拦截：payload 携带 kind 时必须在三枚举内（缺省时由
    // forward-compat merge 沿用盘上的 kind，允许部分更新其他字段）
    if let Some(kind) = obj.get("kind") {
        validate_zcode_kind(kind)?;
    }

    let _guard = zcode_write_lock().lock()?;

    let mut entry = build_provider_entry(obj);

    let mut root = read_zcode_settings()?;
    let providers = ensure_child_object(&mut root, PROVIDER_KEY);

    if let Some(existing) = providers.get(key).and_then(|v| v.as_object()) {
        merge_preserve_unknown(&mut entry, existing);
    }

    // name 兜底：payload 无 displayName 且盘上也没有 name 时回退为 provider key
    entry
        .entry("name".to_string())
        .or_insert_with(|| Value::String(key.to_string()));

    providers.insert(key.to_string(), Value::Object(entry));

    write_json_object_locked(&get_zcode_settings_path(), "providers", &root)
}

/// 删除 `provider.<key>`（不存在时 no-op）。文件其他内容不动。
pub fn remove_provider(key: &str) -> Result<ZcodeWriteOutcome, AppError> {
    let _guard = zcode_write_lock().lock()?;

    let mut root = read_zcode_settings()?;
    let Some(providers) = root.get_mut(PROVIDER_KEY).and_then(|v| v.as_object_mut()) else {
        return Ok(ZcodeWriteOutcome::default());
    };
    if providers.remove(key).is_none() {
        return Ok(ZcodeWriteOutcome::default());
    }

    write_json_object_locked(&get_zcode_settings_path(), "providers", &root)
}

// ============================================================================
// Common Config Snippets
// ============================================================================
//
// zcode 的通用配置片段是 JSON 对象文本，顶层键深合并进 cli/config.json。
// `mcp`（MCP 服务器管理）由 mcp 层维护（后续任务接线），snippet 不允许触碰。

/// 受保护的顶层键：snippet 含这些键时忽略并告警
const PROTECTED_TOP_LEVEL_KEYS: &[&str] = &["mcp"];

fn is_protected_top_level_key(key: &str) -> bool {
    PROTECTED_TOP_LEVEL_KEYS.contains(&key)
}

/// 解析 snippet 为 JSON Object；空 → 空 Object；顶层非 Object 报 Config 错误。
pub(crate) fn parse_common_config_snippet(snippet: &str) -> Result<Map<String, Value>, AppError> {
    if snippet.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(snippet)
        .map_err(|e| AppError::Config(format!("Failed to parse zcode common config: {e}")))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(AppError::Config(
            "zcode common config snippet must be a JSON object".to_string(),
        )),
    }
}

/// 深合并：两边都是 Object 时逐键递归，否则整体替换。
fn deep_merge_json(target: &mut Value, source: &Value) {
    if let (Value::Object(target_map), Value::Object(source_map)) = (&mut *target, source) {
        for (key, value) in source_map {
            match target_map.get_mut(key) {
                Some(existing) => deep_merge_json(existing, value),
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
/// 避免误删用户后改的内容；嵌套 Object 被掏空后连键一起移除。
fn remove_matching_json(target: &mut Value, source: &Value) {
    let (Value::Object(target_map), Value::Object(source_map)) = (&mut *target, source) else {
        return;
    };
    for (key, source_value) in source_map {
        let should_remove = match target_map.get_mut(key) {
            Some(existing) if existing.is_object() && source_value.is_object() => {
                remove_matching_json(existing, source_value);
                existing.as_object().map(|m| m.is_empty()).unwrap_or(false)
            }
            Some(existing) => existing == source_value,
            None => false,
        };
        if should_remove {
            target_map.remove(key);
        }
    }
}

/// config 是否已包含 snippet 的所有键值（递归；Object 只要求子集）
fn json_contains(target: &Value, source: &Value) -> bool {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            source_map.iter().all(|(key, value)| {
                target_map
                    .get(key)
                    .map(|existing| json_contains(existing, value))
                    .unwrap_or(false)
            })
        }
        (target, source) => target == source,
    }
}

/// 将通用配置片段（JSON 对象）逐顶层键深合并进 cli/config.json。
/// 保护键 `mcp` 不合并（忽略并 log::warn）。文件不存在时从空对象起步新建。
pub fn apply_zcode_common_config(snippet: &str) -> Result<(), AppError> {
    let snippet_map = parse_common_config_snippet(snippet)?;
    if snippet_map.is_empty() {
        return Ok(());
    }

    let _guard = zcode_write_lock().lock()?;
    let mut root = read_zcode_cli_config()?;

    for (key, value) in &snippet_map {
        if is_protected_top_level_key(key) {
            log::warn!("zcode common config: ignored protected top-level key '{key}'");
            continue;
        }
        match root.get_mut(key) {
            Some(existing) => deep_merge_json(existing, value),
            None => {
                root.insert(key.clone(), value.clone());
            }
        }
    }

    write_json_object_locked(&get_zcode_cli_config_path(), "cli", &root)?;
    Ok(())
}

/// 按 snippet 的键路径从 cli/config.json 精确移除（仅当现有值与 snippet
/// 值相等时才移除，避免误删用户后改的内容）。保护键同样不触碰。
pub fn remove_zcode_common_config(snippet: &str) -> Result<(), AppError> {
    let snippet_map = parse_common_config_snippet(snippet)?;
    if snippet_map.is_empty() {
        return Ok(());
    }

    let _guard = zcode_write_lock().lock()?;
    let mut root = read_zcode_cli_config()?;

    for (key, value) in &snippet_map {
        if is_protected_top_level_key(key) {
            log::warn!("zcode common config: ignored protected top-level key '{key}'");
            continue;
        }
        let should_remove = match root.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                remove_matching_json(existing, value);
                existing.as_object().map(|m| m.is_empty()).unwrap_or(false)
            }
            Some(existing) => existing == value,
            None => false,
        };
        if should_remove {
            root.remove(key);
        }
    }

    write_json_object_locked(&get_zcode_cli_config_path(), "cli", &root)?;
    Ok(())
}

/// cli/config.json 是否已包含 snippet 的所有键值（保护键不参与判定；
/// snippet 解析失败时返回 false）
pub fn zcode_common_config_applied(snippet: &str) -> bool {
    let Ok(snippet_map) = parse_common_config_snippet(snippet) else {
        return false;
    };
    if snippet_map.is_empty() {
        return true;
    }
    let Ok(root) = read_zcode_cli_config() else {
        return false;
    };
    snippet_map
        .iter()
        .filter(|(key, _)| !is_protected_top_level_key(key))
        .all(|(key, value)| {
            root.get(key)
                .map(|existing| json_contains(existing, value))
                .unwrap_or(false)
        })
}

// ============================================================================
// Validation
// ============================================================================

/// 校验 `kind` 值必须在三枚举内（非法 kind 是 zcode 最大的坑：
/// ZCode safeParse 失败会清空整个 provider 配置）。
fn validate_zcode_kind(value: &Value) -> Result<(), AppError> {
    let valid = value
        .as_str()
        .map(|s| ALLOWED_KINDS.contains(&s))
        .unwrap_or(false);
    if !valid {
        return Err(AppError::localized(
            "provider.zcode.invalidKind",
            format!(
                "kind 必须是以下之一: {}。⚠️ 写入非法 kind 会导致 ZCode 解析失败并清空全部 provider 配置！",
                ALLOWED_KINDS.join(", ")
            ),
            format!(
                "kind must be one of: {}. WARNING: an invalid kind makes ZCode fail to parse the config and wipes ALL provider entries!",
                ALLOWED_KINDS.join(", ")
            ),
        ));
    }
    Ok(())
}

/// 校验 zcode provider key：非空且字符集 `[a-zA-Z0-9_:-]+`（对齐 ZCode 的
/// provider id schema；非法 id 同样会触发解析失败清空配置）。
pub fn validate_zcode_provider_key(key: &str) -> Result<(), AppError> {
    let valid = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '-');
    if !valid {
        return Err(AppError::localized(
            "provider.zcode.invalidProviderKey",
            format!(
                "provider key '{key}' 非法：仅允许字母、数字、'_'、':'、'-'。⚠️ 非法 id 会导致 ZCode 解析失败并清空全部 provider 配置！"
            ),
            format!(
                "Invalid provider key '{key}': only letters, digits, '_', ':' and '-' are allowed. WARNING: an invalid id makes ZCode wipe ALL provider entries!"
            ),
        ));
    }
    Ok(())
}

/// 校验 zcode 供应商配置（settings_config 扁平契约）：
/// 必须是 JSON 对象；`kind` 必填且在三枚举内；`baseURL` 非空；
/// `models`（若有）必须是数组且元素有非空 `id`。
pub fn validate_zcode_provider_config(config: &Value) -> Result<(), AppError> {
    let obj = config.as_object().ok_or_else(|| {
        AppError::localized(
            "provider.zcode.configNotObject",
            "zcode 供应商配置必须是 JSON 对象",
            "zcode provider config must be a JSON object.",
        )
    })?;

    let kind = obj.get("kind").ok_or_else(|| {
        AppError::localized(
            "provider.zcode.kindRequired",
            "kind 必填（anthropic / openai / openai-compatible 之一）",
            "kind is required (one of: anthropic, openai, openai-compatible).",
        )
    })?;
    validate_zcode_kind(kind)?;

    let base_url_ok = obj
        .get("baseURL")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !base_url_ok {
        return Err(AppError::localized(
            "provider.zcode.baseUrlRequired",
            "baseURL 不能为空",
            "baseURL is required.",
        ));
    }

    if let Some(models) = obj.get("models") {
        let Some(arr) = models.as_array() else {
            return Err(AppError::localized(
                "provider.zcode.modelsInvalid",
                "models 必须是数组",
                "models must be an array.",
            ));
        };
        for (index, model) in arr.iter().enumerate() {
            let id_ok = model
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if !id_ok {
                return Err(AppError::localized(
                    "provider.zcode.modelIdRequired",
                    format!("models[{index}] 缺少非空 id"),
                    format!("models[{index}] must have a non-empty id."),
                ));
            }
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

    /// Run a test with an isolated temp home directory.
    ///
    /// Saves and restores `CC_SWITCH_TEST_HOME` to avoid interfering with
    /// parallel tests in other modules. zcode 没有环境变量覆盖层（与 dsh 的
    /// `DSH_HOME` 不同），无需额外中和。
    fn with_test_home<T>(test_fn: impl FnOnce() -> T) -> T {
        let _guard = test_guard();
        let tmp = tempfile::tempdir().unwrap();
        let old_test_home = std::env::var_os("CC_SWITCH_TEST_HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", tmp.path());
        let result = test_fn();
        match old_test_home {
            Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
        result
    }

    /// Seed a v2/config.json with the given raw content.
    fn seed_settings(raw: &str) {
        let path = get_zcode_settings_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, raw).unwrap();
    }

    /// Seed a cli/config.json with the given raw content.
    fn seed_cli_config(raw: &str) {
        let path = get_zcode_cli_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, raw).unwrap();
    }

    fn demo_config() -> Value {
        json!({
            "kind": "openai-compatible",
            "baseURL": "https://api.example.com/v1",
            "apiKey": "sk-test-123",
            "apiKeyRequired": true,
            "headers": { "X-Custom": "yes" },
            "displayName": "Demo",
            "models": [
                {
                    "id": "model-a",
                    "name": "Model A",
                    "reasoning": { "enabled": true },
                    "limit": { "context": 128000, "output": 8192 }
                }
            ],
            "zcode.modified": "2026-01-01T00:00:00Z"
        })
    }

    // ---- path / read basics ----

    #[test]
    #[serial]
    fn read_settings_returns_empty_object_when_missing() {
        with_test_home(|| {
            assert!(read_zcode_settings().unwrap().is_empty());
            assert!(read_zcode_cli_config().unwrap().is_empty());
            assert!(get_providers().unwrap().is_empty());
            assert!(get_provider("ghost").unwrap().is_none());
        });
    }

    #[test]
    #[serial]
    fn read_settings_rejects_malformed_json() {
        with_test_home(|| {
            seed_settings("{ not valid json");
            assert!(read_zcode_settings().is_err());
            assert!(get_providers().is_err());
        });
    }

    // ---- provider CRUD ----

    #[test]
    #[serial]
    fn provider_roundtrip_flat_to_native_and_back() {
        with_test_home(|| {
            set_provider("demo", demo_config()).unwrap();

            // 盘上为 zcode 原生嵌套形态
            let raw = fs::read_to_string(get_zcode_settings_path()).unwrap();
            assert!(raw.ends_with("}\n"), "pretty JSON + trailing newline");
            let doc: Value = serde_json::from_str(&raw).unwrap();
            let entry = &doc["provider"]["demo"];
            assert_eq!(entry["name"], "Demo");
            assert_eq!(entry["kind"], "openai-compatible");
            assert_eq!(entry["source"], "custom");
            assert_eq!(entry["options"]["apiKey"], "sk-test-123");
            assert_eq!(entry["options"]["baseURL"], "https://api.example.com/v1");
            assert_eq!(entry["options"]["apiKeyRequired"], true);
            assert_eq!(entry["options"]["headers"]["X-Custom"], "yes");
            // models 数组转 map（key=id，元素不含 id）
            assert_eq!(entry["models"]["model-a"]["name"], "Model A");
            assert!(entry["models"]["model-a"].get("id").is_none());
            assert_eq!(entry["models"]["model-a"]["reasoning"]["enabled"], true);
            assert_eq!(entry["models"]["model-a"]["limit"]["context"], 128000);
            // 未知 provider 级字段透传
            assert_eq!(entry["zcode.modified"], "2026-01-01T00:00:00Z");

            // get_providers 反向摊平为扁平 settings_config
            let providers = get_providers().unwrap();
            let demo = providers.get("demo").unwrap();
            assert_eq!(demo["kind"], "openai-compatible");
            assert_eq!(demo["displayName"], "Demo");
            assert_eq!(demo["apiKey"], "sk-test-123");
            assert_eq!(demo["baseURL"], "https://api.example.com/v1");
            assert_eq!(demo["apiKeyRequired"], true);
            assert_eq!(demo["headers"]["X-Custom"], "yes");
            assert_eq!(demo["zcode.modified"], "2026-01-01T00:00:00Z");
            // source / options 不导出到扁平形态
            assert!(demo.get("source").is_none());
            assert!(demo.get("options").is_none());
            // models map 转回数组（key 作为元素 id，reasoning/limit 保留）
            let models = demo["models"].as_array().unwrap();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0]["id"], "model-a");
            assert_eq!(models[0]["name"], "Model A");
            assert_eq!(models[0]["reasoning"]["enabled"], true);
            assert_eq!(models[0]["limit"]["output"], 8192);
        });
    }

    #[test]
    #[serial]
    fn set_provider_preserves_unknown_fields_and_top_level_keys() {
        with_test_home(|| {
            seed_settings(
                r#"{
  "provider": {
    "acme": {
      "name": "Acme",
      "kind": "anthropic",
      "source": "custom",
      "options": { "apiKey": "sk-old", "baseURL": "https://old.example.com" },
      "models": { "m1": { "name": "M1" } },
      "zcode.modified": 12345
    }
  },
  "zcode.modified": "top-level-preserved",
  "otherTop": { "x": 1 }
}"#,
            );

            // 部分更新：只改 apiKey（不带 displayName/kind/models）
            set_provider("acme", json!({ "apiKey": "sk-new" })).unwrap();

            let raw = fs::read_to_string(get_zcode_settings_path()).unwrap();
            let doc: Value = serde_json::from_str(&raw).unwrap();
            // 其他顶层键原样保留
            assert_eq!(doc["zcode.modified"], "top-level-preserved");
            assert_eq!(doc["otherTop"]["x"], 1);

            let entry = &doc["provider"]["acme"];
            // options 逐键填充缺口：apiKey 更新，baseURL 沿用盘上
            assert_eq!(entry["options"]["apiKey"], "sk-new");
            assert_eq!(entry["options"]["baseURL"], "https://old.example.com");
            // provider 级未知字段 / name / kind / models 均沿用盘上
            assert_eq!(entry["zcode.modified"], 12345);
            assert_eq!(entry["name"], "Acme");
            assert_eq!(entry["kind"], "anthropic");
            assert_eq!(entry["models"]["m1"]["name"], "M1");
        });
    }

    #[test]
    #[serial]
    fn set_provider_name_falls_back_to_key_on_create() {
        with_test_home(|| {
            set_provider(
                "my-prov",
                json!({ "kind": "openai", "baseURL": "https://a.example.com" }),
            )
            .unwrap();

            let providers = get_providers().unwrap();
            let entry = providers.get("my-prov").unwrap();
            assert_eq!(entry["displayName"], "my-prov");
        });
    }

    #[test]
    #[serial]
    fn set_provider_rejects_invalid_key_and_kind_before_write() {
        with_test_home(|| {
            // 非法 provider key（空格不在 [a-zA-Z0-9_:-] 内）
            let err = set_provider("bad key", demo_config()).expect_err("invalid key");
            assert!(err.to_string().contains("清空"), "醒目提示: {err}");

            // 非法 kind（写盘前拦截，不产生文件）
            let mut config = demo_config();
            config["kind"] = json!("responses");
            let err = set_provider("demo", config).expect_err("invalid kind");
            assert!(err.to_string().contains("清空"), "醒目提示: {err}");

            assert!(!get_zcode_settings_path().exists(), "校验失败不得写盘");
        });
    }

    #[test]
    #[serial]
    fn set_provider_noop_when_content_unchanged() {
        with_test_home(|| {
            let first = set_provider("demo", demo_config()).unwrap();
            assert!(first.backup_path.is_none(), "新建无备份");

            let second = set_provider("demo", demo_config()).unwrap();
            assert!(
                second.backup_path.is_none(),
                "内容一致时 no-op（不备份、不写盘）"
            );
        });
    }

    #[test]
    #[serial]
    fn remove_provider_roundtrip_and_noop() {
        with_test_home(|| {
            set_provider("solo", demo_config()).unwrap();
            assert!(get_provider("solo").unwrap().is_some());

            // 删除已存在的条目：产生备份，其他 provider 不动
            set_provider("other", demo_config()).unwrap();
            let outcome = remove_provider("solo").unwrap();
            assert!(outcome.backup_path.is_some(), "删除前备份");
            assert!(get_provider("solo").unwrap().is_none());
            assert!(get_provider("other").unwrap().is_some());

            // 删除不存在的 key：no-op
            let outcome = remove_provider("ghost").unwrap();
            assert!(outcome.backup_path.is_none());
        });
    }

    #[test]
    #[serial]
    fn remove_provider_missing_file_is_noop() {
        with_test_home(|| {
            let outcome = remove_provider("ghost").unwrap();
            assert!(outcome.backup_path.is_none());
            assert!(!get_zcode_settings_path().exists());
        });
    }

    // ---- backup ----

    #[test]
    #[serial]
    fn write_creates_backup_under_cc_switch_backups_dir() {
        with_test_home(|| {
            seed_settings("{ \"provider\": {} }");
            let outcome = set_provider("demo", demo_config()).unwrap();
            let backup = outcome.backup_path.expect("写前备份已存在文件");
            let backup_path = Path::new(&backup);
            assert!(backup_path.exists());
            let filename = backup_path.file_name().unwrap().to_string_lossy();
            assert!(
                filename.starts_with("zcode_providers_") && filename.ends_with(".json"),
                "备份命名: {filename}"
            );
            // 备份内容是写入前的原文
            assert_eq!(
                fs::read_to_string(backup_path).unwrap(),
                "{ \"provider\": {} }"
            );
        });
    }

    // ---- common config snippets ----

    #[test]
    #[serial]
    fn common_config_apply_contain_remove_roundtrip() {
        with_test_home(|| {
            let snippet =
                r#"{"agent": {"maxTurns": 10, "temperature": 0.5}, "ui": {"theme": "dark"}}"#;
            assert!(!zcode_common_config_applied(snippet));

            apply_zcode_common_config(snippet).unwrap();
            assert!(zcode_common_config_applied(snippet));

            // 文件不存在时 apply 从空对象起步新建
            let root = read_zcode_cli_config().unwrap();
            assert_eq!(root["agent"]["maxTurns"], 10);
            assert_eq!(root["ui"]["theme"], "dark");

            remove_zcode_common_config(snippet).unwrap();
            assert!(!zcode_common_config_applied(snippet));
            let root = read_zcode_cli_config().unwrap();
            assert!(root.get("agent").is_none());
            assert!(root.get("ui").is_none());
        });
    }

    #[test]
    #[serial]
    fn common_config_apply_deep_merges_with_existing() {
        with_test_home(|| {
            seed_cli_config(r#"{"agent": {"maxTurns": 5}}"#);
            apply_zcode_common_config(r#"{"agent": {"temperature": 0.5}}"#).unwrap();

            let root = read_zcode_cli_config().unwrap();
            assert_eq!(root["agent"]["maxTurns"], 5);
            assert_eq!(root["agent"]["temperature"], 0.5);
        });
    }

    #[test]
    #[serial]
    fn common_config_ignores_protected_mcp_key() {
        with_test_home(|| {
            seed_cli_config(r#"{"mcp": {"servers": {"existing": {"command": "x"}}}}"#);

            let snippet = r#"{"mcp": {"servers": {"evil": {"command": "y"}}}, "telemetry": {"enabled": false}}"#;
            apply_zcode_common_config(snippet).unwrap();

            // 保护键未被合并
            let root = read_zcode_cli_config().unwrap();
            assert!(root["mcp"]["servers"].get("evil").is_none());
            assert!(root["mcp"]["servers"].get("existing").is_some());
            // 非保护键正常合并
            assert_eq!(root["telemetry"]["enabled"], false);
            // 保护键不参与 applied 判定
            assert!(zcode_common_config_applied(snippet));

            // remove 同样不触碰保护键
            remove_zcode_common_config(snippet).unwrap();
            let root = read_zcode_cli_config().unwrap();
            assert!(root["mcp"]["servers"].get("existing").is_some());
            assert!(root.get("telemetry").is_none());
        });
    }

    #[test]
    #[serial]
    fn common_config_remove_skips_user_modified_values() {
        with_test_home(|| {
            apply_zcode_common_config(r#"{"ui": {"theme": "dark", "font": "mono"}}"#).unwrap();

            // 用户随后改了 theme
            let mut root = read_zcode_cli_config().unwrap();
            root["ui"]["theme"] = json!("light");
            write_zcode_cli_config(&root).unwrap();

            remove_zcode_common_config(r#"{"ui": {"theme": "dark", "font": "mono"}}"#).unwrap();

            let root = read_zcode_cli_config().unwrap();
            // theme 已被用户改走 → 不误删；font 值未变 → 移除
            assert_eq!(root["ui"]["theme"], "light");
            assert!(root["ui"].get("font").is_none());
        });
    }

    #[test]
    fn common_config_snippet_must_be_object() {
        assert!(parse_common_config_snippet("[1, 2]").is_err());
        assert!(parse_common_config_snippet("").unwrap().is_empty());
        assert!(parse_common_config_snippet("  \n ").unwrap().is_empty());
        assert!(parse_common_config_snippet("not json").is_err());
    }

    // ---- validate_zcode_provider_config / validate_zcode_provider_key ----

    #[test]
    fn validate_accepts_valid_config() {
        let config = json!({
            "kind": "openai-compatible",
            "baseURL": "https://a.example.com",
            "models": [{ "id": "m" }],
        });
        assert!(validate_zcode_provider_config(&config).is_ok());
        // models 可省略
        let config = json!({
            "kind": "anthropic",
            "baseURL": "https://a.example.com",
        });
        assert!(validate_zcode_provider_config(&config).is_ok());
        // models 空数组允许（清空模型列表）
        let config = json!({
            "kind": "openai",
            "baseURL": "https://a.example.com",
            "models": [],
        });
        assert!(validate_zcode_provider_config(&config).is_ok());
    }

    #[test]
    fn validate_rejects_missing_or_invalid_kind() {
        let config = json!({ "baseURL": "https://a.example.com" });
        assert!(validate_zcode_provider_config(&config).is_err());

        let config = json!({ "kind": "responses", "baseURL": "https://a.example.com" });
        let err = validate_zcode_provider_config(&config).expect_err("invalid kind");
        // 醒目提示：非法 kind 会导致 ZCode 清空整个 provider 配置
        assert!(err.to_string().contains("清空"), "醒目提示: {err}");
        assert!(err.to_string().contains("anthropic"), "列出合法值: {err}");
    }

    #[test]
    fn validate_rejects_missing_or_blank_base_url() {
        let config = json!({ "kind": "openai" });
        assert!(validate_zcode_provider_config(&config).is_err());
        let config = json!({ "kind": "openai", "baseURL": "  " });
        assert!(validate_zcode_provider_config(&config).is_err());
    }

    #[test]
    fn validate_rejects_invalid_models() {
        let config = json!({
            "kind": "openai",
            "baseURL": "https://a.example.com",
            "models": { "m": {} },
        });
        assert!(validate_zcode_provider_config(&config).is_err());
        let config = json!({
            "kind": "openai",
            "baseURL": "https://a.example.com",
            "models": [{ "name": "no-id" }],
        });
        assert!(validate_zcode_provider_config(&config).is_err());
    }

    #[test]
    fn validate_provider_key_charset() {
        assert!(validate_zcode_provider_key("builtin:zai").is_ok());
        assert!(validate_zcode_provider_key("my-prov_2").is_ok());
        assert!(validate_zcode_provider_key("").is_err());
        assert!(validate_zcode_provider_key("bad key").is_err());
        assert!(validate_zcode_provider_key("bad.key").is_err());
        assert!(validate_zcode_provider_key("中文").is_err());
    }
}
