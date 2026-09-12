//! zcode（智谱 ZCode）MCP sync and import module
//!
//! zcode 的 MCP 配置在 `<zcode_dir>/cli/config.json` 的 `mcp.servers`
//! （以 server 名为键的 map）。纯 JSON map 操作——没有 dsh cordis.patch.yml
//! 的文本块/自定义标签问题；读写经 `zcode_config::update_zcode_cli_config`
//! （写锁内闭包式读-改-写 + 备份 + atomic_write），与 common config 的
//! cli/config.json 写入共享同一把锁，无 TOCTOU。
//!
//! ## Format mapping
//!
//! | CC Switch unified (JSON)                                | zcode `mcp.servers.<name>`           |
//! |---------------------------------------------------------|--------------------------------------|
//! | `{"type":"stdio","command":..,"args":..,"env":..,"cwd":..}` | `{command, args?, env?, cwd?}`（剥掉 type） |
//! | `{"type":"http"/"sse","url":..,"headers":..}`           | `{type, url, headers?}` 直写         |
//!
//! ## `enable` 语义
//!
//! zcode 在 server 对象里用 `"enable": false` 表示停用（注意是 enable 不是
//! enabled）。cc-switch 的"取消勾选" = 从 map 移除整条（与 opencode 一致，
//! 不写 `enable: false`——那是用户在 zcode 侧自己的状态）；upsert 时沿用
//! 盘上该条目的未知字段（含 `enable`，用户在 zcode 侧的停用状态不被
//! 覆盖，对齐 zcode_config 的 forward-compat merge 哲学）；import 时剥离
//! `enable`（统一结构没有此概念，启用与否由 apps 标志表达）。

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::app_config::{McpApps, McpServer, MultiAppConfig};
use crate::error::AppError;

use super::validation::validate_server_spec;

// ============================================================================
// Helpers
// ============================================================================

/// Check if zcode MCP sync should proceed（与 hermes/dsh 一致：目录不存在
/// 则跳过——不为未安装 zcode 的用户创建 ~/.zcode）
fn should_sync_zcode_mcp() -> bool {
    crate::settings::get_zcode_dir().exists()
}

/// 取 `root["mcp"]["servers"]` 的可变 Object；缺失按需创建，存在但不是
/// Object（损坏）时由 zcode_config::ensure_child_object 告警并重建。
fn ensure_mcp_servers(root: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let mcp = crate::zcode_config::ensure_child_object(root, "mcp");
    crate::zcode_config::ensure_child_object(mcp, "servers")
}

/// Upsert 一个 server 条目：核心字段来自 cc-switch，盘上该条目的未知字段
/// （含 `enable`）原样保留（forward-compat merge）。
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
// Format Conversion: CC Switch -> zcode
// ============================================================================

/// Convert CC Switch unified format to zcode `mcp.servers` entry
///
/// Conversion rules:
/// - `stdio`（或省略 type）：剥掉 `type`，直写 `command`（必填）+ 非空
///   `args`/`env`/`cwd`
/// - `sse`/`http`：直写 `type`/`url`（必填）+ 非空 `headers`
/// - 未知 type 报 `McpValidation`
fn convert_to_zcode_format(id: &str, spec: &Value) -> Result<Value, AppError> {
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
                        "zcode MCP server '{id}' (stdio) requires a non-empty 'command'"
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
        "sse" | "http" => {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::McpValidation(format!(
                        "zcode MCP server '{id}' ({typ}) requires a non-empty 'url'"
                    ))
                })?;
            result.insert("type".into(), json!(typ));
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
// Format Conversion: zcode -> CC Switch
// ============================================================================

/// Convert zcode `mcp.servers` entry to CC Switch unified format
///
/// Conversion rules:
/// - 有 `command`：`type: "stdio"`，提取 `command`/`args`/`env`/`cwd`
/// - 否则有 `url`：`type` 取盘上值（仅认 `http`/`sse`，缺省 `http`），
///   提取 `url`/`headers`
/// - `enable`（zcode 侧停用状态）剥离，不进入统一结构
fn convert_from_zcode_format(name: &str, spec: &Value) -> Result<Value, AppError> {
    let obj = spec
        .as_object()
        .ok_or_else(|| AppError::McpValidation("zcode MCP spec must be a JSON object".into()))?;

    let mut result = Map::new();

    if obj.contains_key("command") {
        // stdio type
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
        // http/sse type（缺省按 http；其他显式 type 保守拒绝）
        let typ = obj.get("type").and_then(|v| v.as_str()).unwrap_or("http");
        if !matches!(typ, "http" | "sse") {
            return Err(AppError::McpValidation(format!(
                "zcode MCP server '{name}' has unsupported type '{typ}'"
            )));
        }
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
            "zcode MCP server '{name}' has neither 'command' nor 'url' field"
        )));
    }

    // Note: zcode-specific `enable` is intentionally NOT copied — it is
    // stripped on import (enablement lives in the per-app flags).

    Ok(Value::Object(result))
}

// ============================================================================
// Public API: Sync Functions
// ============================================================================

/// Sync a single MCP server to zcode live config（upsert `mcp.servers.<id>`，
/// 幂等；保留盘上该条目的未知字段含 `enable`）
pub fn sync_single_server_to_zcode(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_zcode_mcp() {
        return Ok(());
    }

    let zcode_spec = convert_to_zcode_format(id, server_spec)?;

    crate::zcode_config::update_zcode_cli_config(|root| {
        upsert_server_entry(ensure_mcp_servers(root), id, zcode_spec);
        Ok(())
    })?;
    Ok(())
}

/// Remove a single MCP server from zcode live config（只移除该键；
/// `mcp`/`servers` 空壳保留——对齐 opencode 不剪枝空 map 的惯例）
pub fn remove_server_from_zcode(id: &str) -> Result<(), AppError> {
    if !should_sync_zcode_mcp() {
        return Ok(());
    }

    crate::zcode_config::update_zcode_cli_config(|root| {
        if let Some(servers) = root
            .get_mut("mcp")
            .and_then(|v| v.get_mut("servers"))
            .and_then(|v| v.as_object_mut())
        {
            servers.remove(id);
        }
        Ok(())
    })?;
    Ok(())
}

/// 把当前 DB 中所有 server 的 zcode 启用状态投影到 live：启用则 upsert、
/// 未启用则移除该键（map 操作幂等；其他应用/用户手写的条目不动）。
pub fn sync_enabled_to_zcode(servers: &IndexMap<String, McpServer>) -> Result<(), AppError> {
    if !should_sync_zcode_mcp() {
        return Ok(());
    }

    crate::zcode_config::update_zcode_cli_config(|root| {
        let servers_map = ensure_mcp_servers(root);
        for server in servers.values() {
            if server.apps.zcode {
                let spec = convert_to_zcode_format(&server.id, &server.server)?;
                upsert_server_entry(servers_map, &server.id, spec);
            } else {
                servers_map.remove(&server.id);
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Import MCP servers from zcode cli/config.json to unified structure
///
/// `mcp.servers` 的键原样作为统一注册表 id。已存在的 server 仅启用 zcode
/// 应用标记，不覆盖其他字段。无法映射的条目（既无 command 也无 url、
/// 未知 type 等）跳过并 log::warn。
pub fn import_from_zcode(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let root = crate::zcode_config::read_zcode_cli_config()?;
    let servers_map = root
        .get("mcp")
        .and_then(|v| v.get("servers"))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if servers_map.is_empty() {
        return Ok(0);
    }

    // Ensure servers map exists
    let servers = config.mcp.servers.get_or_insert_with(HashMap::new);

    let mut changed = 0;
    let mut errors = Vec::new();

    for (name, spec) in &servers_map {
        let id = name.trim();
        if id.is_empty() {
            log::warn!("Skip zcode MCP server with empty name");
            continue;
        }

        // Convert from zcode format to unified format
        let unified_spec = match convert_from_zcode_format(id, spec) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Skip invalid zcode MCP server '{id}': {e}");
                errors.push(format!("{id}: {e}"));
                continue;
            }
        };

        // Validate the converted spec
        if let Err(e) = validate_server_spec(&unified_spec) {
            log::warn!("Skip invalid MCP server '{id}' after zcode conversion: {e}");
            errors.push(format!("{id}: {e}"));
            continue;
        }

        if let Some(existing) = servers.get_mut(id) {
            // Existing server: just enable zcode app
            if !existing.apps.zcode {
                existing.apps.zcode = true;
                changed += 1;
                log::info!("MCP server '{id}' enabled for zcode");
            }
        } else {
            // New server: default to only zcode enabled
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
                        zcode: true,
                    },
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: Vec::new(),
                },
            );
            changed += 1;
            log::info!("Imported new MCP server '{id}' from zcode");
        }
    }

    if !errors.is_empty() {
        log::warn!(
            "zcode MCP import completed with {} failures: {:?}",
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
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    /// Run a test with an isolated temp home directory（与 zcode_config.rs
    /// 同模式：CC_SWITCH_TEST_HOME 指向临时目录；zcode 无环境变量覆盖层，
    /// 无需额外中和，绝不触碰真实 ~/.zcode）。
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

    /// zcode 目录必须存在（should_sync_zcode_mcp 门控），并可选写入初始
    /// cli/config.json
    fn seed_zcode_dir(raw: Option<&str>) {
        let dir = crate::settings::get_zcode_dir();
        fs::create_dir_all(&dir).unwrap();
        if let Some(raw) = raw {
            let path = crate::zcode_config::get_zcode_cli_config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, raw).unwrap();
        }
    }

    fn read_cli_raw() -> String {
        fs::read_to_string(crate::zcode_config::get_zcode_cli_config_path()).unwrap()
    }

    fn read_cli_json() -> Value {
        serde_json::from_str(&read_cli_raw()).unwrap()
    }

    fn make_server(id: &str, spec: Value, zcode_enabled: bool) -> (String, McpServer) {
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
                    zcode: zcode_enabled,
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
    // sync / remove tests
    // ========================================================================

    #[test]
    #[serial]
    fn sync_single_stdio_server_is_idempotent() {
        with_test_home(|| {
            seed_zcode_dir(None);
            let spec = json!({
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github"],
                "env": { "GITHUB_TOKEN": "token" },
                "cwd": "/tmp/work"
            });

            sync_single_server_to_zcode(&MultiAppConfig::default(), "github", &spec).unwrap();
            let first = read_cli_raw();
            sync_single_server_to_zcode(&MultiAppConfig::default(), "github", &spec).unwrap();
            assert_eq!(first, read_cli_raw(), "upsert must be idempotent");

            let doc = read_cli_json();
            let entry = &doc["mcp"]["servers"]["github"];
            // stdio 剥掉 type 字段，其余直写
            assert!(entry.get("type").is_none());
            assert_eq!(entry["command"], "npx");
            assert_eq!(entry["args"][0], "-y");
            assert_eq!(entry["args"][1], "@modelcontextprotocol/server-github");
            assert_eq!(entry["env"]["GITHUB_TOKEN"], "token");
            assert_eq!(entry["cwd"], "/tmp/work");
        });
    }

    #[test]
    #[serial]
    fn sync_single_http_server_keeps_type_and_url() {
        with_test_home(|| {
            seed_zcode_dir(None);
            let spec = json!({
                "type": "sse",
                "url": "https://example.com/mcp",
                "headers": { "Authorization": "Bearer xxx" }
            });

            sync_single_server_to_zcode(&MultiAppConfig::default(), "remote", &spec).unwrap();

            let doc = read_cli_json();
            let entry = &doc["mcp"]["servers"]["remote"];
            assert_eq!(entry["type"], "sse");
            assert_eq!(entry["url"], "https://example.com/mcp");
            assert_eq!(entry["headers"]["Authorization"], "Bearer xxx");
        });
    }

    #[test]
    #[serial]
    fn sync_omits_empty_args_env_and_headers() {
        with_test_home(|| {
            seed_zcode_dir(None);
            sync_single_server_to_zcode(
                &MultiAppConfig::default(),
                "plain",
                &json!({ "type": "stdio", "command": "node", "args": [], "env": {} }),
            )
            .unwrap();
            sync_single_server_to_zcode(
                &MultiAppConfig::default(),
                "bare-http",
                &json!({ "type": "http", "url": "http://localhost:1/mcp", "headers": {} }),
            )
            .unwrap();

            let doc = read_cli_json();
            let plain = &doc["mcp"]["servers"]["plain"];
            assert!(plain.get("args").is_none());
            assert!(plain.get("env").is_none());
            assert!(doc["mcp"]["servers"]["bare-http"].get("headers").is_none());
        });
    }

    #[test]
    #[serial]
    fn sync_preserves_enable_false_and_unknown_fields() {
        with_test_home(|| {
            // 用户在 zcode 侧停用了该 server（enable:false），并有未来字段
            seed_zcode_dir(Some(
                r#"{
  "mcp": {
    "servers": {
      "github": {
        "command": "old-cmd",
        "enable": false,
        "futureField": { "x": 1 }
      }
    }
  }
}"#,
            ));

            sync_single_server_to_zcode(
                &MultiAppConfig::default(),
                "github",
                &json!({ "type": "stdio", "command": "new-cmd" }),
            )
            .unwrap();

            let doc = read_cli_json();
            let entry = &doc["mcp"]["servers"]["github"];
            // 核心字段被 cc-switch 覆盖
            assert_eq!(entry["command"], "new-cmd");
            // 用户在 zcode 侧的停用状态与未来字段原样保留
            assert_eq!(entry["enable"], false);
            assert_eq!(entry["futureField"]["x"], 1);
        });
    }

    #[test]
    #[serial]
    fn sync_preserves_other_top_level_keys_and_foreign_servers() {
        with_test_home(|| {
            seed_zcode_dir(Some(
                r#"{
  "agent": { "maxTurns": 10 },
  "mcp": {
    "servers": {
      "foreign": { "command": "keep-me", "enable": false }
    }
  }
}"#,
            ));

            sync_single_server_to_zcode(
                &MultiAppConfig::default(),
                "github",
                &json!({ "type": "stdio", "command": "npx" }),
            )
            .unwrap();

            let doc = read_cli_json();
            assert_eq!(doc["agent"]["maxTurns"], 10);
            let foreign = &doc["mcp"]["servers"]["foreign"];
            assert_eq!(foreign["command"], "keep-me");
            assert_eq!(foreign["enable"], false);
            assert_eq!(doc["mcp"]["servers"]["github"]["command"], "npx");
        });
    }

    #[test]
    #[serial]
    fn remove_deletes_only_matching_and_keeps_empty_shell() {
        with_test_home(|| {
            seed_zcode_dir(None);
            let servers = make_server_map(vec![
                make_server("a", json!({ "type": "stdio", "command": "cmd-a" }), true),
                make_server(
                    "b",
                    json!({ "type": "http", "url": "http://localhost:9000/mcp" }),
                    true,
                ),
            ]);
            sync_enabled_to_zcode(&servers).unwrap();

            remove_server_from_zcode("a").unwrap();
            let doc = read_cli_json();
            assert!(doc["mcp"]["servers"].get("a").is_none());
            assert!(doc["mcp"]["servers"].get("b").is_some());

            // 全删后 mcp.servers 空壳保留（opencode 惯例：不剪枝空 map）
            remove_server_from_zcode("b").unwrap();
            let doc = read_cli_json();
            assert!(doc["mcp"]["servers"].as_object().unwrap().is_empty());

            // 再删不存在的是 no-op
            remove_server_from_zcode("ghost").unwrap();
        });
    }

    #[test]
    #[serial]
    fn sync_enabled_removes_unchecked_and_keeps_foreign() {
        with_test_home(|| {
            seed_zcode_dir(Some(
                r#"{ "mcp": { "servers": { "foreign": { "command": "keep" } } } }"#,
            ));
            let servers = make_server_map(vec![
                make_server("a", json!({ "type": "stdio", "command": "cmd-a" }), true),
                make_server("b", json!({ "type": "stdio", "command": "cmd-b" }), true),
            ]);
            sync_enabled_to_zcode(&servers).unwrap();
            let doc = read_cli_json();
            assert_eq!(doc["mcp"]["servers"].as_object().unwrap().len(), 3);

            // b 取消勾选：投影后只剩 a + foreign
            let servers = make_server_map(vec![
                make_server("a", json!({ "type": "stdio", "command": "cmd-a" }), true),
                make_server("b", json!({ "type": "stdio", "command": "cmd-b" }), false),
            ]);
            sync_enabled_to_zcode(&servers).unwrap();
            let doc = read_cli_json();
            let map = doc["mcp"]["servers"].as_object().unwrap();
            assert_eq!(map.len(), 2);
            assert!(map.get("b").is_none(), "取消勾选的 server 必须被移除");
            assert!(map.get("a").is_some());
            assert!(map.get("foreign").is_some());
        });
    }

    #[test]
    #[serial]
    fn sync_rejects_stdio_without_command() {
        with_test_home(|| {
            seed_zcode_dir(None);
            let result = sync_single_server_to_zcode(
                &MultiAppConfig::default(),
                "bad",
                &json!({ "type": "stdio" }),
            );
            assert!(result.is_err());
            assert!(!crate::zcode_config::get_zcode_cli_config_path().exists());
        });
    }

    #[test]
    #[serial]
    fn sync_without_zcode_dir_is_noop() {
        with_test_home(|| {
            // zcode 目录不存在：同步静默跳过（与 hermes/dsh 的门控一致）
            let servers = make_server_map(vec![make_server(
                "github",
                json!({ "type": "stdio", "command": "npx" }),
                true,
            )]);
            sync_enabled_to_zcode(&servers).unwrap();
            assert!(!crate::zcode_config::get_zcode_cli_config_path().exists());
        });
    }

    // ========================================================================
    // import_from_zcode tests
    // ========================================================================

    #[test]
    #[serial]
    fn import_roundtrip_restores_unified_specs() {
        with_test_home(|| {
            seed_zcode_dir(None);
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
            sync_enabled_to_zcode(&servers).unwrap();

            let mut config = MultiAppConfig::default();
            let changed = import_from_zcode(&mut config).unwrap();
            assert_eq!(changed, 2);

            let imported = config.mcp.servers.as_ref().unwrap();
            let github = imported.get("github").unwrap();
            assert!(github.apps.zcode);
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
    fn import_skips_invalid_and_strips_enable() {
        with_test_home(|| {
            seed_zcode_dir(Some(
                r#"{
  "mcp": {
    "servers": {
      "broken": { "note": "neither command nor url" },
      "bad-type": { "type": "grpc", "url": "http://localhost:1" },
      "disabled-one": { "command": "npx", "enable": false }
    }
  }
}"#,
            ));

            let mut config = MultiAppConfig::default();
            let changed = import_from_zcode(&mut config).unwrap();

            // broken / bad-type 跳过；disabled-one 导入但 enable 不进入统一结构
            assert_eq!(changed, 1);
            let imported = config.mcp.servers.as_ref().unwrap();
            assert!(!imported.contains_key("broken"));
            assert!(!imported.contains_key("bad-type"));
            let disabled = imported.get("disabled-one").unwrap();
            assert_eq!(disabled.server["type"], "stdio");
            assert_eq!(disabled.server["command"], "npx");
            assert!(disabled.server.get("enable").is_none());
        });
    }

    #[test]
    #[serial]
    fn import_enables_zcode_flag_on_existing_server() {
        with_test_home(|| {
            seed_zcode_dir(None);
            sync_single_server_to_zcode(
                &MultiAppConfig::default(),
                "github",
                &json!({ "type": "stdio", "command": "npx" }),
            )
            .unwrap();

            // 统一注册表里已有同名 server（其他应用启用中）
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

            let changed = import_from_zcode(&mut config).unwrap();
            assert_eq!(changed, 1);
            let server = config.mcp.servers.as_ref().unwrap().get("github").unwrap();
            assert!(server.apps.zcode);

            // 再导入一次：无变化
            let changed = import_from_zcode(&mut config).unwrap();
            assert_eq!(changed, 0);
        });
    }

    #[test]
    #[serial]
    fn import_missing_file_is_zero() {
        with_test_home(|| {
            let mut config = MultiAppConfig::default();
            assert_eq!(import_from_zcode(&mut config).unwrap(), 0);
        });
    }
}
