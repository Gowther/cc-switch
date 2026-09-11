//! dsh（DeepSeek Harness）MCP sync and import module
//!
//! Handles conversion between CC Switch unified MCP format and dsh's
//! `cordis.patch.yml`（`$DSH_HOME` home 级，对所有 profile 生效）。
//!
//! ## Format mapping
//!
//! cordis.patch.yml 顶层是 patch 条目列表，每台 MCP server 一条 insert 条目：
//!
//! ```yaml
//! - insert:
//!     - id: mcp-<serverName>                      # cc-switch 生成，幂等锚点
//!       name: '@deepseek-ai/dsh-mcp-client'
//!       config:
//!         serverName: <serverName>                # [A-Za-z0-9_-]{1,32}
//!         transport: stdio                        # 或 streamable-http
//!         command: npx                            # stdio 必填
//!         args: ['-y', 'pkg']                     # 可选（空则省略）
//!         env: { KEY: value }                     # 可选（空则省略）
//!         cwd: /path                              # 可选
//! ```
//!
//! | CC Switch unified (JSON)                              | dsh cordis.patch.yml (YAML)          |
//! |-------------------------------------------------------|--------------------------------------|
//! | `{"type":"stdio","command":..,"args":..,"env":..,"cwd":..}` | `transport: stdio` + command/args/env/cwd |
//! | `{"type":"http"/"sse","url":..,"headers":..}`         | `transport: streamable-http` + url/headers |
//!
//! ## 保留语义
//!
//! 文件中可能出现 `!!js` 自定义 YAML 标签（如 `!!js process.env.X`），
//! serde_yaml 解析为 `Value::Tagged`。本模块只增删 cc-switch 管理的条目
//! （`name == '@deepseek-ai/dsh-mcp-client'` 且 id 以 `mcp-` 开头，或
//! serverName 与待写入目标相同），其余条目（含 Tagged 值）原样保留。
//! 导入方向遇到无法安全转成纯字符串的 Tagged env/headers 值时跳过该
//! server 并 log::warn，不做硬转。

use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::app_config::{McpApps, McpServer, MultiAppConfig};
use crate::config::atomic_write;
use crate::error::AppError;

use super::validation::validate_server_spec;

/// dsh MCP client 插件名（cordis.patch.yml 条目里 `name` 的固定值）
const MCP_CLIENT_PLUGIN: &str = "@deepseek-ai/dsh-mcp-client";
/// cc-switch 管理条目的 id 前缀（`mcp-<serverName>`）
const MANAGED_ID_PREFIX: &str = "mcp-";
/// dsh `serverName` 最大长度（`[A-Za-z0-9_-]{1,32}`）
const SERVER_NAME_MAX_LEN: usize = 32;
/// 冲突哈希后缀长度（`-` + 8 位 hex）
const HASH_SUFFIX_LEN: usize = 9;

// ============================================================================
// Path / Lock Helpers
// ============================================================================

/// 获取 dsh home 级 `cordis.patch.yml` 路径
fn get_dsh_cordis_patch_path() -> PathBuf {
    crate::settings::get_dsh_dir().join("cordis.patch.yml")
}

fn dsh_mcp_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Check if dsh MCP sync should proceed（与 hermes 一致：目录不存在则跳过）
fn should_sync_dsh_mcp() -> bool {
    crate::settings::get_dsh_dir().exists()
}

// ============================================================================
// serverName Sanitize
// ============================================================================

/// FNV-1a 32-bit——跨版本稳定的微型哈希（`DefaultHasher` 不保证跨版本一致）
fn fnv1a_32(s: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in s.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// 把任意 cc-switch 条目 id 映射为合法的 dsh `serverName`
/// （`[A-Za-z0-9_-]{1,32}`）：
/// - 合法字符原样保留，非法字符逐个折叠为 `-`
/// - 空输入回退为 `server`
/// - 若 sanitize 未改变 id 且不超长，原样返回（干净 id 保持可读、稳定）
/// - 否则追加确定性哈希后缀（`-<8 hex>`，基名截断到 23 字符），保证
///   `foo.bar` / `foo bar` 这类折叠后同名的 id 仍能区分，且幂等
fn sanitize_server_name(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return "server".to_string();
    }
    let folded: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    if folded == trimmed && folded.len() <= SERVER_NAME_MAX_LEN {
        return folded;
    }

    let keep = SERVER_NAME_MAX_LEN - HASH_SUFFIX_LEN;
    let truncated: String = folded.chars().take(keep).collect();
    let head = truncated.trim_end_matches('-');
    let head = if head.is_empty() { "server" } else { head };
    format!("{head}-{:08x}", fnv1a_32(trimmed))
}

// ============================================================================
// cordis.patch.yml Read/Write（备份机制复用 dsh_config 的 create_dsh_backup）
// ============================================================================

/// 读取 cordis.patch.yml 顶层 patch 条目列表。
///
/// 文件不存在、为空或只含注释（解析为 Null）时返回空列表；
/// 顶层不是列表（配置已损坏，dsh 自身也无法加载）时报 Config 错误而不是覆盖，
/// 避免误毁用户数据。
fn read_patch_entries() -> Result<Vec<serde_yaml::Value>, AppError> {
    let path = get_dsh_cordis_patch_path();
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let value: serde_yaml::Value = serde_yaml::from_str(&content).map_err(|e| {
        AppError::Config(format!("Failed to parse dsh cordis.patch.yml as YAML: {e}"))
    })?;
    match value {
        serde_yaml::Value::Null => Ok(Vec::new()),
        serde_yaml::Value::Sequence(seq) => Ok(seq),
        _ => Err(AppError::Config(
            "dsh cordis.patch.yml top level must be a list of patch entries".to_string(),
        )),
    }
}

/// 写回 cordis.patch.yml（写锁 + 写前备份 + atomic_write）。
///
/// 内容与磁盘一致时 no-op（不备份、不写盘）；条目为空且文件不存在时同样 no-op，
/// 避免为"清空"创建一个无意义的 `[]` 文件。
fn write_patch_entries(entries: &[serde_yaml::Value]) -> Result<(), AppError> {
    let _guard = dsh_mcp_write_lock().lock()?;

    let path = get_dsh_cordis_patch_path();
    if entries.is_empty() && !path.exists() {
        return Ok(());
    }

    let raw = if path.exists() {
        fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?
    } else {
        String::new()
    };

    let serialized = serde_yaml::to_string(&serde_yaml::Value::Sequence(entries.to_vec()))
        .map_err(|e| AppError::Config(format!("Failed to serialize dsh cordis.patch.yml: {e}")))?;

    if serialized == raw {
        return Ok(());
    }

    if !raw.is_empty() {
        crate::dsh_config::create_dsh_backup("cordis", &raw)?;
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }

    atomic_write(&path, serialized.as_bytes())?;
    log::debug!("dsh cordis.patch.yml written to {:?}", path);
    Ok(())
}

// ============================================================================
// Entry / Item Helpers
// ============================================================================

fn yaml_str(s: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(s.to_string())
}

/// insert 条目内单个插件项的 `name`
fn item_plugin_name(item: &serde_yaml::Value) -> Option<&str> {
    item.get("name").and_then(|v| v.as_str())
}

/// insert 条目内单个插件项的 `id`
fn item_id(item: &serde_yaml::Value) -> Option<&str> {
    item.get("id").and_then(|v| v.as_str())
}

/// insert 条目内单个插件项的 `config.serverName`
fn item_server_name(item: &serde_yaml::Value) -> Option<&str> {
    item.get("config")
        .and_then(|c| c.get("serverName"))
        .and_then(|v| v.as_str())
}

/// 是否为 dsh MCP client 插件项（不看 id 前缀——导入时采纳所有插件项）
fn is_mcp_client_item(item: &serde_yaml::Value) -> bool {
    item_plugin_name(item) == Some(MCP_CLIENT_PLUGIN)
}

/// 按条目级 `insert` 列表移除满足条件的插件项；条目被掏空（`insert` 是唯一键
/// 且已空）时连条目一起删除。非 insert 条目与空 insert 的用户条目原样保留。
fn remove_managed_items(
    entries: &mut Vec<serde_yaml::Value>,
    should_remove: impl Fn(&serde_yaml::Value) -> bool,
) {
    entries.retain_mut(|entry| {
        let Some(seq) = entry.get_mut("insert").and_then(|v| v.as_sequence_mut()) else {
            return true;
        };
        let before = seq.len();
        seq.retain(|item| !(is_mcp_client_item(item) && should_remove(item)));
        let emptied = seq.is_empty() && seq.len() < before;
        let only_insert_key = entry.as_mapping().map(|m| m.len() == 1).unwrap_or(false);
        !(emptied && only_insert_key)
    });
}

/// Upsert 一个插件项：按 `id` 或 `config.serverName` 匹配已有插件项原位替换
/// （幂等），否则追加一条新的 `- insert: [item]` 条目。
fn upsert_managed_item(entries: &mut Vec<serde_yaml::Value>, item: serde_yaml::Value) {
    let target_id = item_id(&item).unwrap_or_default().to_string();
    let target_name = item_server_name(&item).unwrap_or_default().to_string();

    for entry in entries.iter_mut() {
        let Some(seq) = entry.get_mut("insert").and_then(|v| v.as_sequence_mut()) else {
            continue;
        };
        for slot in seq.iter_mut() {
            if !is_mcp_client_item(slot) {
                continue;
            }
            let id_match = !target_id.is_empty() && item_id(slot) == Some(target_id.as_str());
            let name_match =
                !target_name.is_empty() && item_server_name(slot) == Some(target_name.as_str());
            if id_match || name_match {
                *slot = item;
                return;
            }
        }
    }

    let entry = serde_yaml::Value::Mapping(serde_yaml::Mapping::from_iter([
        (yaml_str("insert"), serde_yaml::Value::Sequence(vec![item])),
    ]));
    entries.push(entry);
}
// ============================================================================
// Format Conversion: CC Switch -> dsh
// ============================================================================

/// 把 cc-switch 统一格式（JSON）转成 cordis.patch.yml 的插件项（YAML）。
///
/// 转换规则：
/// - `stdio`（或省略 type）：`transport: stdio` + `command`（必填）+
///   非空 `args`/`env`/`cwd`
/// - `sse`/`http`：`transport: streamable-http` + `url`（必填）+ 非空 `headers`
/// - 未知 type 报 `McpValidation`
fn build_insert_item(server_name: &str, spec: &Value) -> Result<serde_yaml::Value, AppError> {
    let obj = spec
        .as_object()
        .ok_or_else(|| AppError::McpValidation("MCP spec must be a JSON object".into()))?;

    let typ = obj.get("type").and_then(|v| v.as_str()).unwrap_or("stdio");

    let mut config = serde_yaml::Mapping::new();
    config.insert(yaml_str("serverName"), yaml_str(server_name));

    match typ {
        "stdio" => {
            let command = obj
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::McpValidation(format!(
                        "dsh MCP server '{server_name}' (stdio) requires a non-empty 'command'"
                    ))
                })?;
            config.insert(yaml_str("transport"), yaml_str("stdio"));
            config.insert(yaml_str("command"), yaml_str(command));

            if let Some(args) = obj.get("args") {
                if args.is_array() && !args.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    config.insert(yaml_str("args"), crate::dsh_config::json_to_yaml(args)?);
                }
            }
            if let Some(env) = obj.get("env") {
                if env.is_object() && !env.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                    config.insert(yaml_str("env"), crate::dsh_config::json_to_yaml(env)?);
                }
            }
            if let Some(cwd) = obj.get("cwd").and_then(|v| v.as_str()) {
                if !cwd.trim().is_empty() {
                    config.insert(yaml_str("cwd"), yaml_str(cwd));
                }
            }
        }
        "sse" | "http" => {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::McpValidation(format!(
                        "dsh MCP server '{server_name}' (streamable-http) requires a non-empty 'url'"
                    ))
                })?;
            config.insert(yaml_str("transport"), yaml_str("streamable-http"));
            config.insert(yaml_str("url"), yaml_str(url));

            if let Some(headers) = obj.get("headers") {
                if headers.is_object() && !headers.as_object().map(|o| o.is_empty()).unwrap_or(true)
                {
                    config.insert(
                        yaml_str("headers"),
                        crate::dsh_config::json_to_yaml(headers)?,
                    );
                }
            }
        }
        _ => {
            return Err(AppError::McpValidation(format!("Unknown MCP type: {typ}")));
        }
    }

    let item = serde_yaml::Value::Mapping(serde_yaml::Mapping::from_iter([
        (yaml_str("id"), yaml_str(&format!("{MANAGED_ID_PREFIX}{server_name}"))),
        (yaml_str("name"), yaml_str(MCP_CLIENT_PLUGIN)),
        (yaml_str("config"), serde_yaml::Value::Mapping(config)),
    ]));
    Ok(item)
}

// ============================================================================
// Format Conversion: dsh -> CC Switch
// ============================================================================

/// 取 YAML mapping 的字符串值表（env/headers 用）。任何值不是纯字符串
/// （`!!js` Tagged、数字、嵌套结构等）都视为"无法安全转换"，整个 server
/// 由调用方跳过——不硬转。
fn yaml_str_map(
    value: &serde_yaml::Value,
    field: &str,
    server_name: &str,
) -> Result<serde_json::Map<String, Value>, AppError> {
    let mapping = value.as_mapping().ok_or_else(|| {
        AppError::McpValidation(format!(
            "dsh MCP server '{server_name}': '{field}' must be a mapping"
        ))
    })?;
    let mut out = serde_json::Map::new();
    for (key, val) in mapping {
        let key_str = key.as_str().ok_or_else(|| {
            AppError::McpValidation(format!(
                "dsh MCP server '{server_name}': '{field}' has a non-string key"
            ))
        })?;
        let val_str = val.as_str().ok_or_else(|| {
            AppError::McpValidation(format!(
                "dsh MCP server '{server_name}': '{field}.{key_str}' is not a plain string (possibly a !!js expression)"
            ))
        })?;
        out.insert(key_str.to_string(), json!(val_str));
    }
    Ok(out)
}

/// 取 YAML 字符串数组（args 用）；非纯字符串元素同样视为不可安全转换。
fn yaml_str_seq(
    value: &serde_yaml::Value,
    field: &str,
    server_name: &str,
) -> Result<Vec<String>, AppError> {
    let seq = value.as_sequence().ok_or_else(|| {
        AppError::McpValidation(format!(
            "dsh MCP server '{server_name}': '{field}' must be a list"
        ))
    })?;
    seq.iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                AppError::McpValidation(format!(
                    "dsh MCP server '{server_name}': '{field}[{index}]' is not a plain string"
                ))
            })
        })
        .collect()
}

/// 把一个 dsh MCP client 插件项反向映射为 cc-switch 统一格式。
///
/// 返回 `(serverName, unified_spec)`；`serverName` 原样作为统一注册表的 id。
fn convert_from_dsh_item(item: &serde_yaml::Value) -> Result<(String, Value), AppError> {
    let config = item
        .get("config")
        .filter(|c| c.is_mapping())
        .ok_or_else(|| {
            AppError::McpValidation("dsh MCP client entry is missing a 'config' mapping".into())
        })?;

    let server_name = config
        .get("serverName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::McpValidation(
                "dsh MCP client entry is missing a valid 'config.serverName'".into(),
            )
        })?
        .to_string();

    let transport = config
        .get("transport")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut spec = serde_json::Map::new();
    match transport.as_str() {
        "stdio" => {
            let command = config
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::McpValidation(format!(
                        "dsh MCP server '{server_name}' (stdio) is missing 'command'"
                    ))
                })?;
            spec.insert("type".into(), json!("stdio"));
            spec.insert("command".into(), json!(command));

            if let Some(args) = config.get("args") {
                let args = yaml_str_seq(args, "args", &server_name)?;
                if !args.is_empty() {
                    spec.insert("args".into(), json!(args));
                }
            }
            if let Some(env) = config.get("env") {
                let env = yaml_str_map(env, "env", &server_name)?;
                if !env.is_empty() {
                    spec.insert("env".into(), Value::Object(env));
                }
            }
            if let Some(cwd_value) = config.get("cwd") {
                let cwd = cwd_value.as_str().ok_or_else(|| {
                    AppError::McpValidation(format!(
                        "dsh MCP server '{server_name}': 'cwd' is not a plain string (possibly a !!js expression)"
                    ))
                })?;
                if !cwd.trim().is_empty() {
                    spec.insert("cwd".into(), json!(cwd));
                }
            }
        }
        "streamable-http" => {
            let url = config
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::McpValidation(format!(
                        "dsh MCP server '{server_name}' (streamable-http) is missing 'url'"
                    ))
                })?;
            spec.insert("type".into(), json!("http"));
            spec.insert("url".into(), json!(url));

            if let Some(headers) = config.get("headers") {
                let headers = yaml_str_map(headers, "headers", &server_name)?;
                if !headers.is_empty() {
                    spec.insert("headers".into(), Value::Object(headers));
                }
            }
        }
        other => {
            return Err(AppError::McpValidation(format!(
                "dsh MCP server '{server_name}' has unknown transport '{other}'"
            )));
        }
    }

    Ok((server_name, Value::Object(spec)))
}

// ============================================================================
// Public API: Sync Functions
// ============================================================================

/// Sync a single MCP server to dsh live config（按 id/serverName upsert，幂等）
pub fn sync_single_server_to_dsh(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_dsh_mcp() {
        return Ok(());
    }

    let server_name = sanitize_server_name(id);
    let item = build_insert_item(&server_name, server_spec)?;

    let mut entries = read_patch_entries()?;
    upsert_managed_item(&mut entries, item);
    write_patch_entries(&entries)
}

/// Remove a single MCP server from dsh live config
pub fn remove_server_from_dsh(id: &str) -> Result<(), AppError> {
    if !should_sync_dsh_mcp() {
        return Ok(());
    }

    let server_name = sanitize_server_name(id);
    let managed_id = format!("{MANAGED_ID_PREFIX}{server_name}");

    let mut entries = read_patch_entries()?;
    if entries.is_empty() {
        return Ok(());
    }
    remove_managed_items(&mut entries, |item| {
        item_id(item) == Some(managed_id.as_str())
            || item_server_name(item) == Some(server_name.as_str())
    });
    write_patch_entries(&entries)
}

/// 重建所有 cc-switch 管理的 dsh MCP 条目（保留非管理条目，含 `!!js` Tagged）。
///
/// 旧管理条目（id 以 `mcp-` 开头的插件项）与 serverName 命中本次目标集的
/// 插件项先整体剥离，再按当前启用清单重建——改名/禁用留下的陈旧条目因此
/// 被清掉，而用户手写的其他条目逐字保留。
pub fn sync_enabled_to_dsh(servers: &IndexMap<String, McpServer>) -> Result<(), AppError> {
    if !should_sync_dsh_mcp() {
        return Ok(());
    }

    let enabled: Vec<&McpServer> = servers.values().filter(|s| s.apps.dsh).collect();
    let target_names: std::collections::HashSet<String> = enabled
        .iter()
        .map(|server| sanitize_server_name(&server.id))
        .collect();

    let mut entries = read_patch_entries()?;
    remove_managed_items(&mut entries, |item| {
        let managed_id = item_id(item)
            .map(|id| id.starts_with(MANAGED_ID_PREFIX))
            .unwrap_or(false);
        let targeted = item_server_name(item)
            .map(|name| target_names.contains(name))
            .unwrap_or(false);
        managed_id || targeted
    });

    for server in enabled {
        let server_name = sanitize_server_name(&server.id);
        let item = build_insert_item(&server_name, &server.server)?;
        upsert_managed_item(&mut entries, item);
    }

    write_patch_entries(&entries)
}

/// Import MCP servers from dsh cordis.patch.yml to unified structure
///
/// 采纳所有 `@deepseek-ai/dsh-mcp-client` 插件项（不论 id 是否 `mcp-` 前缀），
/// `serverName` 原样作为统一注册表 id。已存在的 server 仅启用 dsh 应用标记，
/// 不覆盖其他字段。含 `!!js` Tagged 值等无法安全转换的条目跳过并 log::warn。
pub fn import_from_dsh(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let entries = read_patch_entries()?;
    if entries.is_empty() {
        return Ok(0);
    }

    // Ensure servers map exists
    let servers = config.mcp.servers.get_or_insert_with(HashMap::new);

    let mut changed = 0;
    let mut errors = Vec::new();

    for entry in &entries {
        let Some(items) = entry.get("insert").and_then(|v| v.as_sequence()) else {
            continue;
        };

        for item in items {
            if !is_mcp_client_item(item) {
                continue;
            }

            let (id, unified_spec) = match convert_from_dsh_item(item) {
                Ok(pair) => pair,
                Err(e) => {
                    log::warn!("Skip dsh MCP client entry: {e}");
                    errors.push(e.to_string());
                    continue;
                }
            };

            // Validate the converted spec
            if let Err(e) = validate_server_spec(&unified_spec) {
                log::warn!("Skip invalid MCP server '{id}' after dsh conversion: {e}");
                errors.push(format!("{id}: {e}"));
                continue;
            }

            if let Some(existing) = servers.get_mut(&id) {
                // Existing server: just enable dsh app
                if !existing.apps.dsh {
                    existing.apps.dsh = true;
                    changed += 1;
                    log::info!("MCP server '{id}' enabled for dsh");
                }
            } else {
                // New server: default to only dsh enabled
                servers.insert(
                    id.clone(),
                    McpServer {
                        id: id.clone(),
                        name: id.clone(),
                        server: unified_spec,
                        apps: McpApps {
                            claude: false,
                            codex: false,
                            gemini: false,
                            opencode: false,
                            hermes: false,
                            dsh: true,
                        },
                        description: None,
                        homepage: None,
                        docs: None,
                        tags: Vec::new(),
                    },
                );
                changed += 1;
                log::info!("Imported new MCP server '{id}' from dsh");
            }
        }
    }

    if !errors.is_empty() {
        log::warn!(
            "dsh MCP import completed with {} failures: {:?}",
            errors.len(),
            errors
        );
    }

    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    /// Run a test with an isolated temp home directory（与 dsh_config.rs 同模式：
    /// CC_SWITCH_TEST_HOME 指向临时目录，中和 DSH_HOME，绝不触碰真实 ~/.dsh）。
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

    /// dsh 目录必须存在（should_sync_dsh_mcp 门控），并可选写入初始 cordis.patch.yml
    fn seed_dsh_dir(raw: Option<&str>) {
        let dir = crate::settings::get_dsh_dir();
        fs::create_dir_all(&dir).unwrap();
        if let Some(raw) = raw {
            fs::write(get_dsh_cordis_patch_path(), raw).unwrap();
        }
    }

    fn read_raw() -> String {
        fs::read_to_string(get_dsh_cordis_patch_path()).unwrap()
    }

    fn read_yaml() -> serde_yaml::Value {
        serde_yaml::from_str(&read_raw()).unwrap()
    }

    fn make_server(id: &str, spec: Value, dsh_enabled: bool) -> (String, McpServer) {
        (
            id.to_string(),
            McpServer {
                id: id.to_string(),
                name: id.to_string(),
                server: spec,
                apps: McpApps {
                    claude: false,
                    codex: false,
                    gemini: false,
                    opencode: false,
                    hermes: false,
                    dsh: dsh_enabled,
                },
                description: None,
                homepage: None,
                docs: None,
                tags: Vec::new(),
            },
        )
    }

    fn make_server_map(servers: Vec<(String, McpServer)>) -> IndexMap<String, McpServer> {
        servers.into_iter().collect()
    }

    // ========================================================================
    // sanitize_server_name tests
    // ========================================================================

    #[test]
    fn sanitize_clean_id_passes_through() {
        assert_eq!(sanitize_server_name("github"), "github");
        assert_eq!(sanitize_server_name("my-server_2"), "my-server_2");
        assert_eq!(sanitize_server_name("a".repeat(32)), "a".repeat(32));
    }

    #[test]
    fn sanitize_folds_illegal_chars_with_hash_suffix() {
        let name = sanitize_server_name("foo.bar baz");
        assert!(name.starts_with("foo-bar-baz-"), "got: {name}");
        assert!(name.len() <= SERVER_NAME_MAX_LEN);
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    #[test]
    fn sanitize_truncates_long_ids() {
        let long = "a".repeat(40);
        let name = sanitize_server_name(&long);
        assert_eq!(name.len(), SERVER_NAME_MAX_LEN);
        assert!(name.starts_with("a".repeat(23).as_str()));
    }

    #[test]
    fn sanitize_empty_falls_back_to_server() {
        assert_eq!(sanitize_server_name(""), "server");
        assert_eq!(sanitize_server_name("   "), "server");
    }

    #[test]
    fn sanitize_is_deterministic_and_disambiguates_collisions() {
        assert_eq!(sanitize_server_name("foo.bar"), sanitize_server_name("foo.bar"));
        // 两个折叠后同名的 id 必须因哈希后缀而不同
        assert_ne!(sanitize_server_name("foo.bar"), sanitize_server_name("foo bar"));
        // 干净 id 不带哈希后缀，与脏 id 不冲突
        assert_ne!(sanitize_server_name("foo-bar"), sanitize_server_name("foo.bar"));
    }

    // ========================================================================
    // sync / remove tests
    // ========================================================================

    #[test]
    #[serial]
    fn sync_single_stdio_server_is_idempotent() {
        with_test_home(|| {
            seed_dsh_dir(None);
            let spec = json!({
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github"],
                "env": { "GITHUB_TOKEN": "token" }
            });

            sync_single_server_to_dsh(&MultiAppConfig::default(), "github", &spec).unwrap();
            let first = read_raw();
            sync_single_server_to_dsh(&MultiAppConfig::default(), "github", &spec).unwrap();
            assert_eq!(first, read_raw(), "upsert must be idempotent");

            let yaml = read_yaml();
            let seq = yaml.as_sequence().unwrap();
            assert_eq!(seq.len(), 1);
            let item = &seq[0].get("insert").unwrap().as_sequence().unwrap()[0];
            assert_eq!(item.get("id").unwrap().as_str(), Some("mcp-github"));
            assert_eq!(
                item.get("name").unwrap().as_str(),
                Some("@deepseek-ai/dsh-mcp-client")
            );
            let config = item.get("config").unwrap();
            assert_eq!(config.get("serverName").unwrap().as_str(), Some("github"));
            assert_eq!(config.get("transport").unwrap().as_str(), Some("stdio"));
            assert_eq!(config.get("command").unwrap().as_str(), Some("npx"));
            assert_eq!(config.get("args").unwrap().as_sequence().unwrap().len(), 2);
            assert_eq!(
                config.get("env").unwrap().get("GITHUB_TOKEN").unwrap().as_str(),
                Some("token")
            );
        });
    }

    #[test]
    #[serial]
    fn sync_single_http_server_maps_to_streamable_http() {
        with_test_home(|| {
            seed_dsh_dir(None);
            let spec = json!({
                "type": "sse",
                "url": "https://example.com/mcp",
                "headers": { "Authorization": "Bearer xxx" }
            });

            sync_single_server_to_dsh(&MultiAppConfig::default(), "remote", &spec).unwrap();

            let yaml = read_yaml();
            let seq = yaml.as_sequence().unwrap();
            assert_eq!(seq.len(), 1);
            let item = seq[0].get("insert").unwrap().as_sequence().unwrap()[0].clone();
            let config = item.get("config").unwrap().clone();
            assert_eq!(config.get("transport").unwrap().as_str(), Some("streamable-http"));
            assert_eq!(config.get("url").unwrap().as_str(), Some("https://example.com/mcp"));
            assert_eq!(
                config.get("headers").unwrap().get("Authorization").unwrap().as_str(),
                Some("Bearer xxx")
            );
        });
    }

    #[test]
    #[serial]
    fn sync_enabled_rebuilds_managed_and_preserves_tagged_entries() {
        with_test_home(|| {
            // 预置：一个含 !!js Tagged 值的用户手写条目（非 mcp- 前缀 id），
            // 一个陈旧的 cc-switch 管理条目（mcp-old）
            seed_dsh_dir(Some(
                "\
- insert:
    - id: my-manual
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: manual
        transport: stdio
        command: npx
        env:
          GITHUB_TOKEN: !!js process.env.GITHUB_TOKEN
- insert:
    - id: mcp-old
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: old
        transport: stdio
        command: old-cmd
- id: some-other-plugin
  disabled: true
",
            ));

            let servers = make_server_map(vec![
                make_server(
                    "github",
                    json!({ "type": "stdio", "command": "npx", "args": ["-y", "pkg"] }),
                    true,
                ),
                make_server(
                    "web",
                    json!({ "type": "http", "url": "http://localhost:3000/mcp" }),
                    true,
                ),
                make_server("disabled-one", json!({ "command": "x" }), false),
            ]);
            sync_enabled_to_dsh(&servers).unwrap();

            let yaml = read_yaml();
            let seq = yaml.as_sequence().unwrap();
            // 用户手写条目 + 其他插件 patch 保留；mcp-old 被清掉；新增 github/web
            assert_eq!(seq.len(), 4);

            // 1) 手写条目保留且 !!js 值仍是 Tagged
            let manual = &seq[0].get("insert").unwrap().as_sequence().unwrap()[0];
            assert_eq!(manual.get("id").unwrap().as_str(), Some("my-manual"));
            let tagged = manual
                .get("config")
                .unwrap()
                .get("env")
                .unwrap()
                .get("GITHUB_TOKEN")
                .unwrap();
            assert!(
                matches!(tagged, serde_yaml::Value::Tagged(_)),
                "!!js value must survive as a tagged value, got: {tagged:?}"
            );

            // 2) 陈旧管理条目被移除
            let ids: Vec<&str> = seq
                .iter()
                .filter_map(|e| e.get("insert"))
                .filter_map(|i| i.as_sequence())
                .flatten()
                .filter_map(|item| item.get("id"))
                .filter_map(|id| id.as_str())
                .collect();
            assert!(!ids.contains(&"mcp-old"), "stale managed entry must go: {ids:?}");
            assert!(ids.contains(&"mcp-github"));
            assert!(ids.contains(&"mcp-web"));
            assert!(!ids.contains(&"mcp-disabled-one"));

            // 3) 非 insert 的其他 patch 条目保留
            assert!(seq.iter().any(|e| e.get("disabled").is_some()));
        });
    }

    #[test]
    #[serial]
    fn sync_enabled_twice_is_idempotent() {
        with_test_home(|| {
            seed_dsh_dir(None);
            let servers = make_server_map(vec![make_server(
                "github",
                json!({ "type": "stdio", "command": "npx" }),
                true,
            )]);
            sync_enabled_to_dsh(&servers).unwrap();
            let first = read_raw();
            sync_enabled_to_dsh(&servers).unwrap();
            assert_eq!(first, read_raw(), "rebuild must be idempotent");
        });
    }

    #[test]
    #[serial]
    fn remove_server_deletes_only_matching_entry() {
        with_test_home(|| {
            seed_dsh_dir(None);
            let servers = make_server_map(vec![
                make_server("a", json!({ "type": "stdio", "command": "cmd-a" }), true),
                make_server(
                    "b",
                    json!({ "type": "http", "url": "http://localhost:9000/mcp" }),
                    true,
                ),
            ]);
            sync_enabled_to_dsh(&servers).unwrap();

            remove_server_from_dsh("a").unwrap();

            let yaml = read_yaml();
            let seq = yaml.as_sequence().unwrap();
            assert_eq!(seq.len(), 1);
            let item = &seq[0].get("insert").unwrap().as_sequence().unwrap()[0];
            assert_eq!(item.get("id").unwrap().as_str(), Some("mcp-b"));

            // 再删一次是 no-op（不存在不报错）
            remove_server_from_dsh("a").unwrap();
            remove_server_from_dsh("ghost").unwrap();
            let yaml = read_yaml();
            assert_eq!(yaml.as_sequence().unwrap().len(), 1);
        });
    }

    #[test]
    #[serial]
    fn sync_sanitizes_server_name_and_entry_id() {
        with_test_home(|| {
            seed_dsh_dir(None);
            let spec = json!({ "type": "stdio", "command": "npx" });
            sync_single_server_to_dsh(&MultiAppConfig::default(), "Foo.Bar@2", &spec).unwrap();

            let yaml = read_yaml();
            let entries = yaml.as_sequence().unwrap();
            let item = &entries[0].get("insert").unwrap().as_sequence().unwrap()[0];
            let server_name = item
                .get("config")
                .unwrap()
                .get("serverName")
                .unwrap()
                .as_str()
                .unwrap();
            assert!(server_name.starts_with("Foo-Bar-2-"), "got: {server_name}");
            assert!(server_name.len() <= SERVER_NAME_MAX_LEN);
            let expected_id = format!("mcp-{server_name}");
            assert_eq!(item.get("id").unwrap().as_str(), Some(expected_id.as_str()));
        });
    }

    #[test]
    #[serial]
    fn sync_rejects_stdio_without_command() {
        with_test_home(|| {
            seed_dsh_dir(None);
            let spec = json!({ "type": "stdio" });
            let result = sync_single_server_to_dsh(&MultiAppConfig::default(), "bad", &spec);
            assert!(result.is_err());
            assert!(!get_dsh_cordis_patch_path().exists());
        });
    }

    // ========================================================================
    // import_from_dsh tests
    // ========================================================================

    #[test]
    #[serial]
    fn import_roundtrip_restores_unified_specs() {
        with_test_home(|| {
            seed_dsh_dir(None);
            let stdio_spec = json!({
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "pkg"],
                "env": { "KEY": "value" },
                "cwd": "/tmp/work"
            });
            let http_spec = json!({
                "type": "http",
                "url": "https://example.com/mcp",
                "headers": { "X-Auth": "abc" }
            });
            let servers = make_server_map(vec![
                make_server("github", stdio_spec, true),
                make_server("web", http_spec, true),
            ]);
            sync_enabled_to_dsh(&servers).unwrap();

            let mut config = MultiAppConfig::default();
            let changed = import_from_dsh(&mut config).unwrap();
            assert_eq!(changed, 2);

            let imported = config.mcp.servers.as_ref().unwrap();
            let github = imported.get("github").unwrap();
            assert!(github.apps.dsh);
            assert!(!github.apps.claude);
            assert_eq!(github.server["type"], "stdio");
            assert_eq!(github.server["command"], "npx");
            assert_eq!(github.server["args"][0], "-y");
            assert_eq!(github.server["env"]["KEY"], "value");
            assert_eq!(github.server["cwd"], "/tmp/work");

            let web = imported.get("web").unwrap();
            assert_eq!(web.server["type"], "http");
            assert_eq!(web.server["url"], "https://example.com/mcp");
            assert_eq!(web.server["headers"]["X-Auth"], "abc");
        });
    }

    #[test]
    #[serial]
    fn import_skips_tagged_env_values_and_adopts_manual_entries() {
        with_test_home(|| {
            seed_dsh_dir(Some(
                "\
- insert:
    - id: my-manual
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: manual
        transport: stdio
        command: npx
        env:
          GITHUB_TOKEN: !!js process.env.GITHUB_TOKEN
- insert:
    - id: whatever
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: clean
        transport: streamable-http
        url: http://localhost:3000/mcp
",
            ));

            let mut config = MultiAppConfig::default();
            let changed = import_from_dsh(&mut config).unwrap();

            // manual 条目 env 含 !!js Tagged 值 → 跳过；clean 条目正常导入
            assert_eq!(changed, 1);
            let imported = config.mcp.servers.as_ref().unwrap();
            assert!(!imported.contains_key("manual"));
            let clean = imported.get("clean").unwrap();
            assert_eq!(clean.server["type"], "http");
            assert_eq!(clean.server["url"], "http://localhost:3000/mcp");
        });
    }

    #[test]
    #[serial]
    fn import_enables_dsh_flag_on_existing_server() {
        with_test_home(|| {
            seed_dsh_dir(None);
            sync_single_server_to_dsh(
                &MultiAppConfig::default(),
                "github",
                &json!({ "type": "stdio", "command": "npx" }),
            )
            .unwrap();

            // 统一注册表里已有同名 server（其他应用启用中）
            let mut config = MultiAppConfig::default();
            let (_, existing) =
                make_server("github", json!({ "type": "stdio", "command": "npx" }), false);
            config
                .mcp
                .servers
                .get_or_insert_with(HashMap::new)
                .insert("github".to_string(), existing);

            let changed = import_from_dsh(&mut config).unwrap();
            assert_eq!(changed, 1);
            let server = config.mcp.servers.as_ref().unwrap().get("github").unwrap();
            assert!(server.apps.dsh);

            // 再导入一次：无变化
            let changed = import_from_dsh(&mut config).unwrap();
            assert_eq!(changed, 0);
        });
    }

    #[test]
    #[serial]
    fn import_missing_file_is_zero() {
        with_test_home(|| {
            // 不建 dsh 目录/文件
            let mut config = MultiAppConfig::default();
            assert_eq!(import_from_dsh(&mut config).unwrap(), 0);
        });
    }

    #[test]
    #[serial]
    fn sync_without_dsh_dir_is_noop() {
        with_test_home(|| {
            // dsh 目录不存在：同步静默跳过（与 hermes should_sync 门控一致）
            let servers = make_server_map(vec![make_server(
                "github",
                json!({ "type": "stdio", "command": "npx" }),
                true,
            )]);
            sync_enabled_to_dsh(&servers).unwrap();
            assert!(!get_dsh_cordis_patch_path().exists());
        });
    }
}
