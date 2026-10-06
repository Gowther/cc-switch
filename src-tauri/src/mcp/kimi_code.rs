//! Kimi Code CLI MCP sync and import module
//!
//! Kimi Code 的 MCP 配置在 `<kimi_dir>/mcp.json` 的 `mcpServers`（以 server
//! 名为键的 map）。纯 JSON map 操作；读写经
//! `kimi_code_config::update_kimi_code_mcp_json`（写锁内闭包式读-改-写 +
//! 备份 + atomic_write）。
//!
//! ## Format mapping
//!
//! | CC Switch unified (JSON)                                    | Kimi Code `mcpServers.<name>`             |
//! |-------------------------------------------------------------|-------------------------------------------|
//! | `{"type":"stdio","command":..,"args":..,"env":..,"cwd":..}` | `{command, args?, env?, cwd?}`（剥掉 type） |
//! | `{"type":"http","url":..,"headers":..}`                     | `{url, headers?}`（http 无 type 字段）      |
//! | `{"type":"sse","url":..,"headers":..}`                      | `{transport: "sse", url, headers?}`         |
//!
//! ## `enabled` 语义
//!
//! Kimi Code 在 server 对象里用 `"enabled": false` 表示停用。cc-switch 的
//! "取消勾选" = 从 map 移除整条（不写 `enabled: false`——那是用户在 Kimi
//! Code 侧自己的状态）；upsert 时保留盘上该条目的未知字段（含 `enabled`、
//! `bearerTokenEnvVar`、超时与工具过滤字段）；import 时剥离这些 Kimi 特有
//! 字段（统一结构没有对应概念）。

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::app_config::{McpApps, McpServer, MultiAppConfig};
use crate::error::AppError;

use super::validation::validate_server_spec;

// ============================================================================
// Helpers
// ============================================================================

/// 与 zcode/hermes 一致：目录不存在则跳过（不为未安装 Kimi Code 的用户创建目录）
fn should_sync_kimi_code_mcp() -> bool {
    crate::settings::get_kimi_code_dir().exists()
}

/// 取 `root["mcpServers"]` 的可变 Object；缺失或类型不符时重建
fn ensure_mcp_servers(root: &mut Value) -> &mut Map<String, Value> {
    if root.get("mcpServers").and_then(|v| v.as_object()).is_none() {
        root["mcpServers"] = json!({});
    }
    root["mcpServers"].as_object_mut().expect("just ensured")
}

/// Upsert 一个 server 条目：核心字段来自 cc-switch，盘上该条目的未知字段
/// （含 `enabled`、`bearerTokenEnvVar`、超时/工具过滤字段）原样保留。
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
// Format Conversion: CC Switch -> Kimi Code
// ============================================================================

/// Convert CC Switch unified format to Kimi Code `mcpServers` entry
fn convert_to_kimi_code_format(id: &str, spec: &Value) -> Result<Value, AppError> {
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
                        "Kimi Code MCP server '{id}' (stdio) requires a non-empty 'command'"
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
                        "Kimi Code MCP server '{id}' ({typ}) requires a non-empty 'url'"
                    ))
                })?;
            // http 不写任何 transport 字段；sse 显式写 transport
            if typ == "sse" {
                result.insert("transport".into(), json!("sse"));
            }
            result.insert("url".into(), json!(url));

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
// Format Conversion: Kimi Code -> CC Switch
// ============================================================================

/// Kimi Code 特有、不进入统一结构的字段
const KIMI_SPECIFIC_KEYS: &[&str] = &[
    "enabled",
    "bearerTokenEnvVar",
    "startupTimeoutMs",
    "toolTimeoutMs",
    "enabledTools",
    "disabledTools",
];

/// Convert Kimi Code `mcpServers` entry to CC Switch unified format
fn convert_from_kimi_code_format(name: &str, spec: &Value) -> Result<Value, AppError> {
    let obj = spec.as_object().ok_or_else(|| {
        AppError::McpValidation("Kimi Code MCP spec must be a JSON object".into())
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
    } else if obj.contains_key("url") {
        let transport = obj.get("transport").and_then(|v| v.as_str());
        let typ = match transport {
            None => "http",
            Some("sse") => "sse",
            Some(other) => {
                return Err(AppError::McpValidation(format!(
                    "Kimi Code MCP server '{name}' has unsupported transport '{other}'"
                )))
            }
        };
        result.insert("type".into(), json!(typ));

        if let Some(url) = obj.get("url") {
            result.insert("url".into(), url.clone());
        }
        if let Some(headers) = obj.get("headers") {
            if headers.is_object() && !headers.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                result.insert("headers".into(), headers.clone());
            }
        }
    } else {
        return Err(AppError::McpValidation(format!(
            "Kimi Code MCP server '{name}' has neither 'command' nor 'url' field"
        )));
    }

    // Kimi 特有字段（enabled / bearerTokenEnvVar / 超时 / 工具过滤）不进入统一结构
    let _ = KIMI_SPECIFIC_KEYS;

    Ok(Value::Object(result))
}

// ============================================================================
// Public API: Sync Functions
// ============================================================================

/// Sync a single MCP server to Kimi Code live config（upsert，幂等）
pub fn sync_single_server_to_kimi_code(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_kimi_code_mcp() {
        return Ok(());
    }

    let kimi_spec = convert_to_kimi_code_format(id, server_spec)?;

    crate::kimi_code_config::update_kimi_code_mcp_json(|root| {
        upsert_server_entry(ensure_mcp_servers(root), id, kimi_spec);
        Ok(())
    })?;
    Ok(())
}

/// Remove a single MCP server from Kimi Code live config（只移除该键）
pub fn remove_server_from_kimi_code(id: &str) -> Result<(), AppError> {
    if !should_sync_kimi_code_mcp() {
        return Ok(());
    }

    crate::kimi_code_config::update_kimi_code_mcp_json(|root| {
        if let Some(servers) = root.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
            servers.remove(id);
        }
        Ok(())
    })?;
    Ok(())
}

/// 投影所有 server 的启用状态：启用则 upsert、未启用则移除该键
pub fn sync_enabled_to_kimi_code(servers: &IndexMap<String, McpServer>) -> Result<(), AppError> {
    if !should_sync_kimi_code_mcp() {
        return Ok(());
    }

    crate::kimi_code_config::update_kimi_code_mcp_json(|root| {
        let servers_map = ensure_mcp_servers(root);
        for server in servers.values() {
            if server.apps.kimi_code {
                let spec = convert_to_kimi_code_format(&server.id, &server.server)?;
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

/// Import MCP servers from Kimi Code mcp.json to unified structure
pub fn import_from_kimi_code(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let path = crate::kimi_code_config::get_kimi_code_mcp_path();
    if !path.exists() {
        return Ok(0);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    if content.trim().is_empty() {
        return Ok(0);
    }
    let root: Value = serde_json::from_str(&content)
        .map_err(|e| AppError::Config(format!("Failed to parse Kimi Code mcp.json: {e}")))?;

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
            log::warn!("Skip Kimi Code MCP server with empty name");
            continue;
        }

        let unified_spec = match convert_from_kimi_code_format(id, spec) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Skip invalid Kimi Code MCP server '{id}': {e}");
                errors.push(format!("{id}: {e}"));
                continue;
            }
        };

        if let Err(e) = validate_server_spec(&unified_spec) {
            log::warn!("Skip invalid MCP server '{id}' after Kimi Code conversion: {e}");
            errors.push(format!("{id}: {e}"));
            continue;
        }

        if let Some(existing) = servers.get_mut(id) {
            if !existing.apps.kimi_code {
                existing.apps.kimi_code = true;
                changed += 1;
                log::info!("MCP server '{id}' enabled for Kimi Code");
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
                        kimi_code: true,
                        antigravity: false,
                    },
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: Vec::new(),
                },
            );
            changed += 1;
            log::info!("Imported new MCP server '{id}' from Kimi Code");
        }
    }

    if !errors.is_empty() {
        log::warn!(
            "Kimi Code MCP import completed with {} failures: {:?}",
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

    fn seed_kimi_dir(raw: Option<&str>) {
        let dir = crate::settings::get_kimi_code_dir();
        fs::create_dir_all(&dir).unwrap();
        if let Some(raw) = raw {
            fs::write(crate::kimi_code_config::get_kimi_code_mcp_path(), raw).unwrap();
        }
    }

    fn read_mcp_json() -> Value {
        let path = crate::kimi_code_config::get_kimi_code_mcp_path();
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn make_server(id: &str, spec: Value, enabled: bool) -> (String, McpServer) {
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
                    dsh: false,
                    zcode: false,
                    kimi_code: enabled,
                    antigravity: false,
                },
                description: None,
                homepage: None,
                docs: None,
                tags: Vec::new(),
            },
        )
    }

    #[test]
    #[serial]
    fn sync_stdio_is_idempotent_and_strips_type() {
        with_test_home(|| {
            seed_kimi_dir(None);
            let spec = json!({
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "pkg"],
                "env": { "KEY": "value" },
                "cwd": "/tmp/work"
            });

            sync_single_server_to_kimi_code(&MultiAppConfig::default(), "github", &spec).unwrap();
            let first =
                fs::read_to_string(crate::kimi_code_config::get_kimi_code_mcp_path()).unwrap();
            sync_single_server_to_kimi_code(&MultiAppConfig::default(), "github", &spec).unwrap();
            let second =
                fs::read_to_string(crate::kimi_code_config::get_kimi_code_mcp_path()).unwrap();
            assert_eq!(first, second, "upsert must be idempotent");

            let doc = read_mcp_json();
            let entry = &doc["mcpServers"]["github"];
            assert!(entry.get("type").is_none());
            assert_eq!(entry["command"], "npx");
            assert_eq!(entry["args"][0], "-y");
            assert_eq!(entry["env"]["KEY"], "value");
            assert_eq!(entry["cwd"], "/tmp/work");
        });
    }

    #[test]
    #[serial]
    fn sync_http_has_no_type_and_sse_has_transport() {
        with_test_home(|| {
            seed_kimi_dir(None);
            sync_single_server_to_kimi_code(
                &MultiAppConfig::default(),
                "web",
                &json!({ "type": "http", "url": "https://example.com/mcp" }),
            )
            .unwrap();
            sync_single_server_to_kimi_code(
                &MultiAppConfig::default(),
                "legacy",
                &json!({ "type": "sse", "url": "https://example.com/sse" }),
            )
            .unwrap();

            let doc = read_mcp_json();
            let web = &doc["mcpServers"]["web"];
            assert!(web.get("transport").is_none());
            assert_eq!(web["url"], "https://example.com/mcp");
            let legacy = &doc["mcpServers"]["legacy"];
            assert_eq!(legacy["transport"], "sse");
            assert_eq!(legacy["url"], "https://example.com/sse");
        });
    }

    #[test]
    #[serial]
    fn sync_preserves_enabled_false_and_foreign_entries() {
        with_test_home(|| {
            seed_kimi_dir(Some(
                r#"{
  "mcpServers": {
    "github": { "command": "old", "enabled": false },
    "foreign": { "command": "keep-me" }
  },
  "other": true
}"#,
            ));

            sync_single_server_to_kimi_code(
                &MultiAppConfig::default(),
                "github",
                &json!({ "type": "stdio", "command": "new-cmd" }),
            )
            .unwrap();

            let doc = read_mcp_json();
            let github = &doc["mcpServers"]["github"];
            assert_eq!(github["command"], "new-cmd");
            assert_eq!(github["enabled"], false, "用户停用状态保留");
            assert_eq!(doc["mcpServers"]["foreign"]["command"], "keep-me");
            assert_eq!(doc["other"], true);
        });
    }

    #[test]
    #[serial]
    fn sync_enabled_removes_unchecked() {
        with_test_home(|| {
            seed_kimi_dir(Some(
                r#"{ "mcpServers": { "foreign": { "command": "keep" } } }"#,
            ));
            let servers: IndexMap<String, McpServer> = vec![
                make_server("a", json!({ "type": "stdio", "command": "cmd-a" }), true),
                make_server("b", json!({ "type": "stdio", "command": "cmd-b" }), false),
            ]
            .into_iter()
            .collect();
            sync_enabled_to_kimi_code(&servers).unwrap();

            let doc = read_mcp_json();
            let map = doc["mcpServers"].as_object().unwrap();
            assert!(map.get("a").is_some());
            assert!(map.get("b").is_none());
            assert!(map.get("foreign").is_some());
        });
    }

    #[test]
    #[serial]
    fn remove_deletes_only_matching() {
        with_test_home(|| {
            seed_kimi_dir(None);
            sync_single_server_to_kimi_code(
                &MultiAppConfig::default(),
                "a",
                &json!({ "type": "stdio", "command": "cmd-a" }),
            )
            .unwrap();
            sync_single_server_to_kimi_code(
                &MultiAppConfig::default(),
                "b",
                &json!({ "type": "stdio", "command": "cmd-b" }),
            )
            .unwrap();

            remove_server_from_kimi_code("a").unwrap();
            let doc = read_mcp_json();
            assert!(doc["mcpServers"].get("a").is_none());
            assert!(doc["mcpServers"].get("b").is_some());

            remove_server_from_kimi_code("ghost").unwrap();
        });
    }

    #[test]
    #[serial]
    fn sync_without_kimi_dir_is_noop() {
        with_test_home(|| {
            let servers: IndexMap<String, McpServer> = vec![make_server(
                "a",
                json!({ "type": "stdio", "command": "cmd" }),
                true,
            )]
            .into_iter()
            .collect();
            sync_enabled_to_kimi_code(&servers).unwrap();
            assert!(!crate::kimi_code_config::get_kimi_code_mcp_path().exists());
        });
    }

    #[test]
    #[serial]
    fn import_roundtrip_and_strips_kimi_specific() {
        with_test_home(|| {
            seed_kimi_dir(Some(
                r#"{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "pkg"], "enabled": false, "toolTimeoutMs": 5000 },
    "web": { "url": "https://example.com/mcp", "headers": { "X-Auth": "abc" } },
    "legacy": { "transport": "sse", "url": "https://example.com/sse" },
    "broken": { "note": "neither command nor url" }
  }
}"#,
            ));

            let mut config = MultiAppConfig::default();
            let changed = import_from_kimi_code(&mut config).unwrap();
            assert_eq!(changed, 3);

            let imported = config.mcp.servers.as_ref().unwrap();
            assert!(!imported.contains_key("broken"));

            let github = imported.get("github").unwrap();
            assert!(github.apps.kimi_code);
            assert_eq!(github.server["type"], "stdio");
            assert_eq!(github.server["command"], "npx");
            assert!(github.server.get("enabled").is_none());
            assert!(github.server.get("toolTimeoutMs").is_none());

            let web = imported.get("web").unwrap();
            assert_eq!(web.server["type"], "http");
            assert_eq!(web.server["headers"]["X-Auth"], "abc");

            let legacy = imported.get("legacy").unwrap();
            assert_eq!(legacy.server["type"], "sse");
        });
    }

    #[test]
    #[serial]
    fn import_enables_flag_on_existing_and_is_idempotent() {
        with_test_home(|| {
            seed_kimi_dir(None);
            sync_single_server_to_kimi_code(
                &MultiAppConfig::default(),
                "github",
                &json!({ "type": "stdio", "command": "npx" }),
            )
            .unwrap();

            let mut config = MultiAppConfig::default();
            let (_, existing) = make_server(
                "github",
                json!({ "type": "stdio", "command": "npx" }),
                false,
            );
            config
                .mcp
                .servers
                .get_or_insert_with(HashMap::new)
                .insert("github".to_string(), existing);

            assert_eq!(import_from_kimi_code(&mut config).unwrap(), 1);
            assert_eq!(import_from_kimi_code(&mut config).unwrap(), 0);
        });
    }
}
