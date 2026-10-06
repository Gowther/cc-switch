//! Antigravity（Google）MCP sync and import module
//!
//! Antigravity 产品族的共享 MCP 配置在 `~/.gemini/config/mcp_config.json`
//! 的 `mcpServers`（以 server 名为键的 map）。纯 JSON map 操作；读写经
//! `antigravity_config::update_antigravity_mcp_config`（写锁内闭包式
//! 读-改-写 + 备份 + atomic_write）。
//!
//! ## Format mapping
//!
//! | CC Switch unified (JSON)                                    | Antigravity `mcpServers.<name>`               |
//! |-------------------------------------------------------------|---------------------------------------------|
//! | `{"type":"stdio","command":..,"args":..,"env":..,"cwd":..}` | `{command, args?, env?, cwd?}`（剥掉 type）   |
//! | `{"type":"http"/"sse","url":..,"headers":..}`               | `{serverUrl, headers?}`（注意不是 `url`！）     |
//!
//! ## `disabled` 语义
//!
//! Antigravity 在 server 对象里用 `"disabled": true` 表示停用。cc-switch 的
//! "取消勾选" = 从 map 移除整条（不写 `disabled`——那是用户在 Antigravity
//! 侧自己的状态）；upsert 保留盘上该条目的未知字段（含 `disabled`、
//! `authProviderType`、`oauth`）；import 时剥离这些 Antigravity 特有字段。
//!
//! 注意：Antigravity GUI 不热监听该文件，写入后用户需在 MCP 面板手动
//! Refresh 或重启（模块头注释见 antigravity_config.rs）。

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::app_config::{McpApps, McpServer, MultiAppConfig};
use crate::error::AppError;

use super::validation::validate_server_spec;

// ============================================================================
// Helpers
// ============================================================================

/// 与其他应用一致：目录不存在则跳过（不为未安装 Antigravity 的用户创建目录）
fn should_sync_antigravity_mcp() -> bool {
    crate::settings::get_antigravity_dir().exists()
}

/// 取 `root["mcpServers"]` 的可变 Object；缺失或类型不符时重建
fn ensure_mcp_servers(root: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let needs_reset = root.get("mcpServers").and_then(|v| v.as_object()).is_none();
    if needs_reset {
        root.insert("mcpServers".to_string(), json!({}));
    }
    root.get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
        .expect("just ensured")
}

/// Upsert 一个 server 条目：核心字段来自 cc-switch，盘上该条目的未知字段
/// （含 `disabled`、`authProviderType`、`oauth`）原样保留。
fn upsert_server_entry(servers: &mut Map<String, Value>, id: &str, mut spec: Value) {
    if let (Some(new_obj), Some(existing)) = (
        spec.as_object_mut(),
        servers.get(id).and_then(|v| v.as_object()),
    ) {
        for (key, value) in existing {
            new_obj.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    servers.insert(id.to_string(), spec);
}

// ============================================================================
// Format Conversion: CC Switch -> Antigravity
// ============================================================================

/// Convert CC Switch unified format to Antigravity `mcpServers` entry
fn convert_to_antigravity_format(id: &str, spec: &Value) -> Result<Value, AppError> {
    let obj = spec
        .as_object()
        .ok_or_else(|| AppError::McpValidation("MCP spec must be a JSON object".into()))?;

    let typ = obj.get("type").and_then(|v| v.as_str()).unwrap_or("stdio");

    let mut result = Map::new();

    match typ {
        "stdio" => {
            let command = obj
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::McpValidation(format!(
                        "Antigravity MCP server '{id}' (stdio) requires a non-empty 'command'"
                    ))
                })?;
            result.insert("command".into(), json!(command));

            if let Some(args) = obj.get("args") {
                if args.is_array() && !args.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    result.insert("args".into(), args.clone());
                }
            }
            if let Some(env) = obj.get("env") {
                if env.is_object() && !env.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                    result.insert("env".into(), env.clone());
                }
            }
            if let Some(cwd) = obj.get("cwd").and_then(|v| v.as_str()) {
                if !cwd.trim().is_empty() {
                    result.insert("cwd".into(), json!(cwd));
                }
            }
        }
        "http" | "sse" => {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::McpValidation(format!(
                        "Antigravity MCP server '{id}' ({typ}) requires a non-empty 'url'"
                    ))
                })?;
            // Antigravity 的远程端点键是 serverUrl（Streamable HTTP/SSE 不分）
            result.insert("serverUrl".into(), json!(url));

            if let Some(headers) = obj.get("headers") {
                if headers.is_object() && !headers.as_object().map(|o| o.is_empty()).unwrap_or(true)
                {
                    result.insert("headers".into(), headers.clone());
                }
            }
        }
        _ => {
            return Err(AppError::McpValidation(format!("Unknown MCP type: {typ}")));
        }
    }

    Ok(Value::Object(result))
}

// ============================================================================
// Format Conversion: Antigravity -> CC Switch
// ============================================================================

/// Antigravity 特有、不进入统一结构的字段
const ANTIGRAVITY_SPECIFIC_KEYS: &[&str] = &["disabled", "authProviderType", "oauth"];

/// Convert Antigravity `mcpServers` entry to CC Switch unified format
fn convert_from_antigravity_format(name: &str, spec: &Value) -> Result<Value, AppError> {
    let obj = spec.as_object().ok_or_else(|| {
        AppError::McpValidation("Antigravity MCP spec must be a JSON object".into())
    })?;

    let mut result = Map::new();

    if obj.contains_key("command") {
        result.insert("type".into(), json!("stdio"));

        if let Some(command) = obj.get("command") {
            result.insert("command".into(), command.clone());
        }
        if let Some(args) = obj.get("args") {
            if args.is_array() && !args.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                result.insert("args".into(), args.clone());
            }
        }
        if let Some(env) = obj.get("env") {
            if env.is_object() && !env.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                result.insert("env".into(), env.clone());
            }
        }
        if let Some(cwd) = obj.get("cwd") {
            if cwd.is_string() && !cwd.as_str().map(|s| s.trim().is_empty()).unwrap_or(true) {
                result.insert("cwd".into(), cwd.clone());
            }
        }
    } else if obj.contains_key("serverUrl") {
        // Antigravity 的 serverUrl 不区分 http/sse，统一映射为 http
        result.insert("type".into(), json!("http"));

        if let Some(url) = obj.get("serverUrl") {
            result.insert("url".into(), url.clone());
        }
        if let Some(headers) = obj.get("headers") {
            if headers.is_object() && !headers.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                result.insert("headers".into(), headers.clone());
            }
        }
    } else {
        return Err(AppError::McpValidation(format!(
            "Antigravity MCP server '{name}' has neither 'command' nor 'serverUrl' field"
        )));
    }

    // Antigravity 特有字段（disabled / authProviderType / oauth）不进入统一结构
    let _ = ANTIGRAVITY_SPECIFIC_KEYS;

    Ok(Value::Object(result))
}

// ============================================================================
// Public API: Sync Functions
// ============================================================================

/// Sync a single MCP server to Antigravity live config（upsert，幂等）
pub fn sync_single_server_to_antigravity(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_antigravity_mcp() {
        return Ok(());
    }

    let ag_spec = convert_to_antigravity_format(id, server_spec)?;

    crate::antigravity_config::update_antigravity_mcp_config(|root| {
        upsert_server_entry(ensure_mcp_servers(root), id, ag_spec);
        Ok(())
    })?;
    Ok(())
}

/// Remove a single MCP server from Antigravity live config（只移除该键）
pub fn remove_server_from_antigravity(id: &str) -> Result<(), AppError> {
    if !should_sync_antigravity_mcp() {
        return Ok(());
    }

    crate::antigravity_config::update_antigravity_mcp_config(|root| {
        if let Some(servers) = root.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
            servers.remove(id);
        }
        Ok(())
    })?;
    Ok(())
}

/// 投影所有 server 的启用状态：启用则 upsert、未启用则移除该键
pub fn sync_enabled_to_antigravity(servers: &IndexMap<String, McpServer>) -> Result<(), AppError> {
    if !should_sync_antigravity_mcp() {
        return Ok(());
    }

    crate::antigravity_config::update_antigravity_mcp_config(|root| {
        let servers_map = ensure_mcp_servers(root);
        for server in servers.values() {
            if server.apps.antigravity {
                let spec = convert_to_antigravity_format(&server.id, &server.server)?;
                upsert_server_entry(servers_map, &server.id, spec);
            } else {
                servers_map.remove(&server.id);
            }
        }
        Ok(())
    })?;
    Ok(())
}

// ============================================================================
// Public API: Import
// ============================================================================

/// Import MCP servers from Antigravity mcp_config.json to unified structure
pub fn import_from_antigravity(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let root = crate::antigravity_config::read_antigravity_mcp_config()?;

    let servers_map = root
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if servers_map.is_empty() {
        return Ok(0);
    }

    let servers = config.mcp.servers.get_or_insert_with(HashMap::new);

    let mut changed = 0;
    let mut errors = Vec::new();

    for (name, spec) in &servers_map {
        let id = name.trim();
        if id.is_empty() {
            log::warn!("Skip Antigravity MCP server with empty name");
            continue;
        }

        let unified_spec = match convert_from_antigravity_format(id, spec) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Skip invalid Antigravity MCP server '{id}': {e}");
                errors.push(format!("{id}: {e}"));
                continue;
            }
        };

        if let Err(e) = validate_server_spec(&unified_spec) {
            log::warn!("Skip invalid MCP server '{id}' after Antigravity conversion: {e}");
            errors.push(format!("{id}: {e}"));
            continue;
        }

        if let Some(existing) = servers.get_mut(id) {
            if !existing.apps.antigravity {
                existing.apps.antigravity = true;
                changed += 1;
                log::info!("MCP server '{id}' enabled for Antigravity");
            }
        } else {
            servers.insert(
                id.to_string(),
                McpServer {
                    id: id.to_string(),
                    name: id.to_string(),
                    server: unified_spec,
                    apps: McpApps {
                        claude: false,
                        codex: false,
                        gemini: false,
                        opencode: false,
                        hermes: false,
                        dsh: false,
                        zcode: false,
                        kimi_code: false,
                        antigravity: true,
                    },
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: Vec::new(),
                },
            );
            changed += 1;
            log::info!("Imported new MCP server '{id}' from Antigravity");
        }
    }

    if !errors.is_empty() {
        log::warn!(
            "Antigravity MCP import completed with {} failures: {:?}",
            errors.len(),
            errors
        );
    }

    Ok(changed)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

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

    fn seed_ag_dir(raw: Option<&str>) {
        let dir = crate::settings::get_antigravity_dir();
        fs::create_dir_all(&dir).unwrap();
        if let Some(raw) = raw {
            fs::write(crate::antigravity_config::get_antigravity_mcp_path(), raw).unwrap();
        }
    }

    fn read_mcp_json() -> Value {
        let path = crate::antigravity_config::get_antigravity_mcp_path();
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    #[serial]
    fn sync_stdio_and_server_url_forms() {
        with_test_home(|| {
            seed_ag_dir(None);
            sync_single_server_to_antigravity(
                &MultiAppConfig::default(),
                "github",
                &json!({ "type": "stdio", "command": "npx", "args": ["-y", "pkg"] }),
            )
            .unwrap();
            sync_single_server_to_antigravity(
                &MultiAppConfig::default(),
                "web",
                &json!({ "type": "http", "url": "https://example.com/mcp", "headers": { "A": "b" } }),
            )
            .unwrap();

            let doc = read_mcp_json();
            let github = &doc["mcpServers"]["github"];
            assert!(github.get("type").is_none());
            assert_eq!(github["command"], "npx");
            // http 的端点必须写成 serverUrl 而不是 url
            let web = &doc["mcpServers"]["web"];
            assert_eq!(web["serverUrl"], "https://example.com/mcp");
            assert!(web.get("url").is_none());
            assert_eq!(web["headers"]["A"], "b");
        });
    }

    #[test]
    #[serial]
    fn sync_is_idempotent_and_preserves_disabled_and_foreign() {
        with_test_home(|| {
            seed_ag_dir(Some(
                r#"{
  "mcpServers": {
    "github": { "command": "old", "disabled": true },
    "foreign": { "command": "keep-me" }
  }
}"#,
            ));
            let spec = json!({ "type": "stdio", "command": "new-cmd" });
            sync_single_server_to_antigravity(&MultiAppConfig::default(), "github", &spec).unwrap();
            let first =
                fs::read_to_string(crate::antigravity_config::get_antigravity_mcp_path()).unwrap();
            sync_single_server_to_antigravity(&MultiAppConfig::default(), "github", &spec).unwrap();
            let second =
                fs::read_to_string(crate::antigravity_config::get_antigravity_mcp_path()).unwrap();
            assert_eq!(first, second);

            let doc = read_mcp_json();
            let github = &doc["mcpServers"]["github"];
            assert_eq!(github["command"], "new-cmd");
            assert_eq!(github["disabled"], true, "用户停用状态保留");
            assert_eq!(doc["mcpServers"]["foreign"]["command"], "keep-me");
        });
    }

    #[test]
    #[serial]
    fn remove_and_sync_enabled_projection() {
        with_test_home(|| {
            seed_ag_dir(None);
            let servers: IndexMap<String, McpServer> = vec![
                (
                    "a".to_string(),
                    McpServer {
                        id: "a".to_string(),
                        name: "a".to_string(),
                        server: json!({ "type": "stdio", "command": "cmd-a" }),
                        apps: McpApps {
                            claude: false,
                            codex: false,
                            gemini: false,
                            opencode: false,
                            hermes: false,
                            dsh: false,
                            zcode: false,
                            kimi_code: false,
                            antigravity: true,
                        },
                        description: None,
                        homepage: None,
                        docs: None,
                        tags: Vec::new(),
                    },
                ),
                (
                    "b".to_string(),
                    McpServer {
                        id: "b".to_string(),
                        name: "b".to_string(),
                        server: json!({ "type": "stdio", "command": "cmd-b" }),
                        apps: McpApps {
                            claude: false,
                            codex: false,
                            gemini: false,
                            opencode: false,
                            hermes: false,
                            dsh: false,
                            zcode: false,
                            kimi_code: false,
                            antigravity: false,
                        },
                        description: None,
                        homepage: None,
                        docs: None,
                        tags: Vec::new(),
                    },
                ),
            ]
            .into_iter()
            .collect();
            sync_enabled_to_antigravity(&servers).unwrap();

            let doc = read_mcp_json();
            let map = doc["mcpServers"].as_object().unwrap();
            assert!(map.get("a").is_some());
            assert!(map.get("b").is_none(), "未启用的必须移除");

            remove_server_from_antigravity("a").unwrap();
            let doc = read_mcp_json();
            assert!(doc["mcpServers"].as_object().unwrap().is_empty());
            remove_server_from_antigravity("ghost").unwrap();
        });
    }

    #[test]
    #[serial]
    fn import_roundtrip_strips_antigravity_specific() {
        with_test_home(|| {
            seed_ag_dir(Some(
                r#"{
  "mcpServers": {
    "github": { "command": "npx", "disabled": true, "authProviderType": "google_credentials" },
    "web": { "serverUrl": "https://example.com/mcp", "headers": { "X-Auth": "abc" } },
    "broken": { "note": "neither command nor serverUrl" }
  }
}"#,
            ));

            let mut config = MultiAppConfig::default();
            let changed = import_from_antigravity(&mut config).unwrap();
            assert_eq!(changed, 2);

            let imported = config.mcp.servers.as_ref().unwrap();
            assert!(!imported.contains_key("broken"));

            let github = imported.get("github").unwrap();
            assert!(github.apps.antigravity);
            assert_eq!(github.server["type"], "stdio");
            assert!(github.server.get("disabled").is_none());
            assert!(github.server.get("authProviderType").is_none());

            let web = imported.get("web").unwrap();
            assert_eq!(web.server["type"], "http");
            assert_eq!(web.server["url"], "https://example.com/mcp");
            assert_eq!(web.server["headers"]["X-Auth"], "abc");
        });
    }

    #[test]
    #[serial]
    fn sync_without_dir_is_noop() {
        with_test_home(|| {
            sync_single_server_to_antigravity(
                &MultiAppConfig::default(),
                "a",
                &json!({ "type": "stdio", "command": "cmd" }),
            )
            .unwrap();
            assert!(!crate::antigravity_config::get_antigravity_mcp_path().exists());
        });
    }
}
