mod support;

use cc_switch_lib::{
    import_from_zcode, remove_server_from_zcode, sync_single_server_to_zcode, update_settings,
    zcode_config, AppSettings, MultiAppConfig,
};
use serde_json::{json, Value};

/// 在隔离的临时 zcode 目录下执行测试：
/// - HOME 指向测试目录（support::ensure_test_home + CC_SWITCH_TEST_HOME）；
/// - `zcode_config_dir` override 指向测试目录下的 `.zcode-roundtrip`，优先级高于
///   默认 `~/.zcode`，因此绝不读写真实 zcode 目录（zcode 无环境变量覆盖层，
///   与 dsh 的 `DSH_HOME` 不同，无需额外中和）；
/// - 结束时恢复设置并清理目录（含 panic 路径）。
fn with_temp_zcode_dir<F: FnOnce(&std::path::Path)>(f: F) {
    let guard = support::test_mutex().lock().expect("test mutex poisoned");
    let home = support::ensure_test_home();
    support::reset_test_fs();

    let zcode_dir = home.join(".zcode-roundtrip");
    let _ = std::fs::remove_dir_all(&zcode_dir);
    std::fs::create_dir_all(&zcode_dir).expect("create temp zcode dir");

    update_settings(AppSettings {
        zcode_config_dir: Some(zcode_dir.to_string_lossy().into_owned()),
        ..AppSettings::default()
    })
    .expect("set zcode_config_dir override");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&zcode_dir)));

    // Always restore settings and drop the fixture dir, even on test failure.
    let _ = update_settings(AppSettings::default());
    let _ = std::fs::remove_dir_all(&zcode_dir);
    drop(guard);

    if let Err(err) = result {
        std::panic::resume_unwind(err);
    }
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json file"))
        .expect("parse json file")
}

// ============================================================================
// Provider roundtrip（扁平 settings_config ↔ zcode 原生嵌套条目）
// ============================================================================

#[test]
fn provider_roundtrip_flat_to_native_and_back_then_remove() {
    with_temp_zcode_dir(|dir| {
        zcode_config::set_provider(
            "demo",
            json!({
                "kind": "openai-compatible",
                "baseURL": "https://api.example.com/v1",
                "apiKey": "sk-demo",
                "displayName": "Demo",
                "models": [{ "id": "model-a", "name": "Model A" }],
            }),
        )
        .expect("set_provider");

        // 盘上为 zcode 原生嵌套形态：连接键收进 options、source 固定 custom、
        // models 数组转 map（key=id，元素内不再带 id）
        let doc = read_json(&dir.join("v2").join("config.json"));
        let entry = &doc["provider"]["demo"];
        assert_eq!(entry["name"], "Demo");
        assert_eq!(entry["kind"], "openai-compatible");
        assert_eq!(entry["source"], "custom");
        assert_eq!(entry["options"]["apiKey"], "sk-demo");
        assert_eq!(entry["options"]["baseURL"], "https://api.example.com/v1");
        assert_eq!(entry["models"]["model-a"]["name"], "Model A");
        assert!(
            entry["models"]["model-a"].get("id").is_none(),
            "models map 元素不应再带 id 键"
        );

        // get_providers 反向摊平为扁平 settings_config
        let providers = zcode_config::get_providers().expect("get_providers");
        let demo = providers.get("demo").expect("demo provider missing");
        assert_eq!(demo["kind"], "openai-compatible");
        assert_eq!(demo["displayName"], "Demo");
        assert_eq!(demo["apiKey"], "sk-demo");
        assert_eq!(demo["baseURL"], "https://api.example.com/v1");
        assert!(
            demo.get("source").is_none() && demo.get("options").is_none(),
            "source/options 不导出到扁平形态"
        );
        let models = demo["models"].as_array().expect("models array");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "model-a");
        assert_eq!(models[0]["name"], "Model A");

        // remove_provider 清除条目，其他内容不动
        zcode_config::remove_provider("demo").expect("remove_provider");
        assert!(
            zcode_config::get_providers()
                .expect("get_providers after remove")
                .get("demo")
                .is_none(),
            "provider must be removed from v2/config.json"
        );
    });
}

#[test]
fn set_provider_preserves_unknown_fields_and_top_level_keys() {
    with_temp_zcode_dir(|dir| {
        let v2_dir = dir.join("v2");
        std::fs::create_dir_all(&v2_dir).expect("create v2 dir");
        std::fs::write(
            v2_dir.join("config.json"),
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
  "otherTop": { "x": 1 }
}"#,
        )
        .expect("seed v2/config.json");

        // 部分更新：只改 apiKey（不带 displayName/kind/models）
        zcode_config::set_provider("acme", json!({ "apiKey": "sk-new" }))
            .expect("partial set_provider");

        let doc = read_json(&v2_dir.join("config.json"));
        // 文件顶层其他键原样保留
        assert_eq!(doc["otherTop"]["x"], 1);

        let entry = &doc["provider"]["acme"];
        // provider 级未知字段保留
        assert_eq!(entry["zcode.modified"], 12345);
        // options 逐键填充缺口：apiKey 更新、baseURL 沿用盘上
        assert_eq!(entry["options"]["apiKey"], "sk-new");
        assert_eq!(entry["options"]["baseURL"], "https://old.example.com");
        // 未提交的字段沿用盘上
        assert_eq!(entry["name"], "Acme");
        assert_eq!(entry["kind"], "anthropic");
        assert_eq!(entry["models"]["m1"]["name"], "M1");
    });
}

// ============================================================================
// Validation（非法 kind 会让 ZCode safeParse 失败并清空整个 provider 配置）
// ============================================================================

#[test]
fn validate_rejects_invalid_kind_with_wipe_warning() {
    let valid = json!({
        "kind": "anthropic",
        "baseURL": "https://a.example.com",
        "models": [{ "id": "m" }],
    });
    assert!(zcode_config::validate_zcode_provider_config(&valid).is_ok());

    let err = zcode_config::validate_zcode_provider_config(&json!({
        "kind": "responses",
        "baseURL": "https://a.example.com",
    }))
    .expect_err("invalid kind must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("清空"), "错误信息应提及清空风险: {msg}");
    assert!(msg.contains("anthropic"), "错误信息应列出合法值: {msg}");

    // kind 必填
    assert!(
        zcode_config::validate_zcode_provider_config(&json!({
            "baseURL": "https://a.example.com"
        }))
        .is_err(),
        "missing kind must be rejected"
    );
    // baseURL 非空
    assert!(
        zcode_config::validate_zcode_provider_config(&json!({
            "kind": "openai",
            "baseURL": "  "
        }))
        .is_err(),
        "blank baseURL must be rejected"
    );
}

#[test]
fn set_provider_rejects_invalid_key_charset() {
    with_temp_zcode_dir(|dir| {
        // 空格不在 [a-zA-Z0-9_:-] 内
        let err = zcode_config::set_provider(
            "bad key",
            json!({
                "kind": "openai",
                "baseURL": "https://a.example.com",
            }),
        )
        .expect_err("invalid provider key must be rejected");
        assert!(
            err.to_string().contains("清空"),
            "错误信息应提及清空风险: {err}"
        );
        assert!(
            !dir.join("v2").join("config.json").exists(),
            "校验失败不得写盘"
        );

        // 合法字符集（含 ':' / '_' / '-'）可写
        zcode_config::set_provider(
            "builtin:zai_2-x",
            json!({
                "kind": "openai",
                "baseURL": "https://a.example.com",
            }),
        )
        .expect("valid key charset should be accepted");
    });
}

// ============================================================================
// MCP: cli/config.json 的 mcp.servers
// ============================================================================

#[test]
fn sync_single_stdio_and_http_servers_structure_and_idempotent() {
    with_temp_zcode_dir(|dir| {
        let stdio_spec = json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "pkg"],
            "env": { "KEY": "value" },
        });
        let http_spec = json!({
            "type": "http",
            "url": "https://example.com/mcp",
            "headers": { "X-Auth": "abc" },
        });

        sync_single_server_to_zcode(&MultiAppConfig::default(), "github", &stdio_spec)
            .expect("sync stdio");
        sync_single_server_to_zcode(&MultiAppConfig::default(), "web", &http_spec)
            .expect("sync http");

        let cli_path = dir.join("cli").join("config.json");
        let first = std::fs::read_to_string(&cli_path).expect("read cli/config.json");

        // 再 sync 一次：幂等，文件内容不变
        sync_single_server_to_zcode(&MultiAppConfig::default(), "github", &stdio_spec)
            .expect("sync stdio again");
        sync_single_server_to_zcode(&MultiAppConfig::default(), "web", &http_spec)
            .expect("sync http again");
        assert_eq!(
            first,
            std::fs::read_to_string(&cli_path).expect("read cli/config.json again"),
            "second sync must be a no-op"
        );

        let doc = read_json(&cli_path);
        let servers = &doc["mcp"]["servers"];
        // stdio：剥掉 type，command/args/env 直写
        let github = &servers["github"];
        assert!(github.get("type").is_none(), "stdio 条目不带 type 键");
        assert_eq!(github["command"], "npx");
        assert_eq!(github["args"][0], "-y");
        assert_eq!(github["env"]["KEY"], "value");
        // http：type/url/headers 直写
        let web = &servers["web"];
        assert_eq!(web["type"], "http");
        assert_eq!(web["url"], "https://example.com/mcp");
        assert_eq!(web["headers"]["X-Auth"], "abc");
    });
}

#[test]
fn remove_server_deletes_only_matching_and_keeps_other_top_level_keys() {
    with_temp_zcode_dir(|dir| {
        // 预置：其他顶层键（hooks）+ 用户自有的 foreign 条目（zcode 侧停用中）
        let cli_dir = dir.join("cli");
        std::fs::create_dir_all(&cli_dir).expect("create cli dir");
        std::fs::write(
            cli_dir.join("config.json"),
            r#"{
  "hooks": { "pre": ["lint"] },
  "mcp": {
    "servers": {
      "foreign": { "command": "keep-me", "enable": false }
    }
  }
}"#,
        )
        .expect("seed cli/config.json");

        sync_single_server_to_zcode(
            &MultiAppConfig::default(),
            "github",
            &json!({ "type": "stdio", "command": "npx" }),
        )
        .expect("sync github");

        let doc = read_json(&cli_dir.join("config.json"));
        assert_eq!(doc["hooks"]["pre"][0], "lint", "其他顶层键必须原样保留");
        assert_eq!(doc["mcp"]["servers"]["foreign"]["command"], "keep-me");
        assert_eq!(doc["mcp"]["servers"]["github"]["command"], "npx");

        // remove：只删目标条目，foreign 与 hooks 不动
        remove_server_from_zcode("github").expect("remove github");
        let doc = read_json(&cli_dir.join("config.json"));
        assert!(doc["mcp"]["servers"].get("github").is_none());
        assert_eq!(doc["mcp"]["servers"]["foreign"]["command"], "keep-me");
        assert_eq!(doc["hooks"]["pre"][0], "lint");

        // 再删不存在的是 no-op
        remove_server_from_zcode("ghost").expect("remove ghost is no-op");
    });
}

#[test]
fn import_from_zcode_roundtrip_restores_specs_and_strips_enable() {
    with_temp_zcode_dir(|dir| {
        let cli_dir = dir.join("cli");
        std::fs::create_dir_all(&cli_dir).expect("create cli dir");
        std::fs::write(
            cli_dir.join("config.json"),
            r#"{
  "mcp": {
    "servers": {
      "github": { "command": "npx", "args": ["-y", "pkg"], "env": { "KEY": "value" } },
      "web": { "type": "sse", "url": "https://example.com/mcp", "headers": { "X-Auth": "abc" } },
      "disabled-one": { "command": "old-cmd", "enable": false }
    }
  }
}"#,
        )
        .expect("seed cli/config.json");

        let mut config = MultiAppConfig::default();
        let changed = import_from_zcode(&mut config).expect("import_from_zcode");
        assert_eq!(changed, 3, "all three servers should be imported");

        let servers = config.mcp.servers.as_ref().expect("servers map");
        assert_eq!(servers.len(), 3);

        // stdio：无 type 的条目补回 type: stdio
        let github = servers.get("github").expect("github server");
        assert!(github.apps.zcode, "imported server enables zcode");
        assert!(
            !github.apps.claude && !github.apps.codex && !github.apps.dsh,
            "imported server defaults to zcode-only"
        );
        assert_eq!(github.server["type"], "stdio");
        assert_eq!(github.server["command"], "npx");
        assert_eq!(github.server["args"][0], "-y");
        assert_eq!(github.server["env"]["KEY"], "value");

        // sse：type 原样保留，url/headers 提取
        let web = servers.get("web").expect("web server");
        assert_eq!(web.server["type"], "sse");
        assert_eq!(web.server["url"], "https://example.com/mcp");
        assert_eq!(web.server["headers"]["X-Auth"], "abc");

        // enable:false 的条目正常导入，但 enable 被剥离（启用与否由 apps 标志表达）
        let disabled = servers.get("disabled-one").expect("disabled-one server");
        assert_eq!(disabled.server["type"], "stdio");
        assert_eq!(disabled.server["command"], "old-cmd");
        assert!(
            disabled.server.get("enable").is_none(),
            "enable must be stripped from the unified spec"
        );
        assert!(disabled.apps.zcode);
    });
}

// ============================================================================
// Common config snippets（cli/config.json，保护键 mcp）
// ============================================================================

#[test]
fn common_config_apply_applied_and_remove_roundtrip() {
    with_temp_zcode_dir(|_dir| {
        let snippet = r#"{"agent": {"maxTurns": 10}, "ui": {"theme": "dark"}}"#;
        assert!(
            !zcode_config::zcode_common_config_applied(snippet),
            "snippet must not be applied initially"
        );

        zcode_config::apply_zcode_common_config(snippet).expect("apply_zcode_common_config");
        assert!(
            zcode_config::zcode_common_config_applied(snippet),
            "snippet must be reported as applied after apply"
        );

        let root = zcode_config::read_zcode_cli_config().expect("read cli config");
        assert_eq!(root["agent"]["maxTurns"], 10);
        assert_eq!(root["ui"]["theme"], "dark");

        zcode_config::remove_zcode_common_config(snippet).expect("remove_zcode_common_config");
        assert!(
            !zcode_config::zcode_common_config_applied(snippet),
            "snippet must not be applied after remove"
        );
        let root = zcode_config::read_zcode_cli_config().expect("read cli config");
        assert!(root.get("agent").is_none(), "applied key must be gone");
        assert!(root.get("ui").is_none(), "applied key must be gone");
    });
}

#[test]
fn common_config_ignores_protected_mcp_key() {
    with_temp_zcode_dir(|dir| {
        let cli_dir = dir.join("cli");
        std::fs::create_dir_all(&cli_dir).expect("create cli dir");
        std::fs::write(
            cli_dir.join("config.json"),
            r#"{ "mcp": { "servers": { "existing": { "command": "x" } } } }"#,
        )
        .expect("seed cli/config.json");

        let snippet =
            r#"{"mcp": {"servers": {"evil": {"command": "y"}}}, "telemetry": {"enabled": false}}"#;
        zcode_config::apply_zcode_common_config(snippet).expect("apply");

        let root = zcode_config::read_zcode_cli_config().expect("read cli config");
        // 保护键 mcp 未被合并
        assert!(root["mcp"]["servers"].get("evil").is_none());
        assert!(root["mcp"]["servers"].get("existing").is_some());
        // 非保护键正常合并
        assert_eq!(root["telemetry"]["enabled"], false);
        // 保护键不参与 applied 判定
        assert!(zcode_config::zcode_common_config_applied(snippet));

        // remove 同样不触碰保护键
        zcode_config::remove_zcode_common_config(snippet).expect("remove");
        let root = zcode_config::read_zcode_cli_config().expect("read cli config");
        assert!(root["mcp"]["servers"].get("existing").is_some());
        assert!(root.get("telemetry").is_none());
    });
}

// ============================================================================
// Backup
// ============================================================================

#[test]
fn set_provider_creates_providers_backup_for_existing_file() {
    with_temp_zcode_dir(|dir| {
        let v2_dir = dir.join("v2");
        std::fs::create_dir_all(&v2_dir).expect("create v2 dir");
        std::fs::write(v2_dir.join("config.json"), "{ \"provider\": {} }")
            .expect("seed v2/config.json");

        let outcome = zcode_config::set_provider(
            "demo",
            json!({
                "kind": "openai",
                "baseURL": "https://a.example.com",
            }),
        )
        .expect("set_provider");

        let backup = outcome.backup_path.expect("写前备份已存在文件");
        let backup_path = std::path::Path::new(&backup);
        assert!(backup_path.exists(), "backup file must exist");
        let filename = backup_path.file_name().unwrap().to_string_lossy();
        assert!(
            filename.starts_with("zcode_providers_") && filename.ends_with(".json"),
            "providers 类备份命名: {filename}"
        );
        // 备份落在 cc-switch 配置目录的 backups/zcode 下（测试 HOME 内，无泄漏）
        let expected_dir = dir
            .parent()
            .expect("zcode dir parent")
            .join(".cc-switch")
            .join("backups")
            .join("zcode");
        assert_eq!(
            backup_path.parent(),
            Some(expected_dir.as_path()),
            "backup must live under the isolated cc-switch backups dir"
        );
        // 备份内容是写入前的原文
        assert_eq!(
            std::fs::read_to_string(backup_path).expect("read backup"),
            "{ \"provider\": {} }"
        );
    });
}
