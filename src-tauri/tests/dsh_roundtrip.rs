mod support;

use cc_switch_lib::{
    dsh_config, import_from_dsh, remove_server_from_dsh, sync_enabled_to_dsh,
    sync_single_server_to_dsh, update_settings, AppSettings, McpApps, McpServer, MultiAppConfig,
};
use indexmap::IndexMap;
use serde_json::json;

/// 在隔离的临时 dsh 目录下执行测试：
/// - HOME 指向测试目录（support::ensure_test_home + CC_SWITCH_TEST_HOME）；
/// - `dsh_config_dir` override 指向测试目录下的 `.dsh-roundtrip`，优先级高于
///   `DSH_HOME` 与默认 `~/.dsh`，因此绝不读写真实 dsh 目录；
/// - 额外中和 `DSH_HOME`（双保险，覆盖未走 override 的代码路径）；
/// - 结束时恢复设置、清理目录并还原环境变量（含 panic 路径）。
fn with_temp_dsh_dir<F: FnOnce(&std::path::Path)>(f: F) {
    let guard = support::test_mutex().lock().expect("test mutex poisoned");
    let home = support::ensure_test_home();
    support::reset_test_fs();

    let old_dsh_home = std::env::var_os("DSH_HOME");
    std::env::remove_var("DSH_HOME");

    let dsh_dir = home.join(".dsh-roundtrip");
    let _ = std::fs::remove_dir_all(&dsh_dir);
    std::fs::create_dir_all(&dsh_dir).expect("create temp dsh dir");

    update_settings(AppSettings {
        dsh_config_dir: Some(dsh_dir.to_string_lossy().into_owned()),
        ..AppSettings::default()
    })
    .expect("set dsh_config_dir override");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dsh_dir)));

    // Always restore settings/env and drop the fixture dir, even on test failure.
    let _ = update_settings(AppSettings::default());
    let _ = std::fs::remove_dir_all(&dsh_dir);
    match old_dsh_home {
        Some(value) => std::env::set_var("DSH_HOME", value),
        None => std::env::remove_var("DSH_HOME"),
    }
    drop(guard);

    if let Err(err) = result {
        std::panic::resume_unwind(err);
    }
}

fn make_dsh_server(id: &str, spec: serde_json::Value, dsh_enabled: bool) -> (String, McpServer) {
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

fn read_yaml(path: &std::path::Path) -> serde_yaml::Value {
    serde_yaml::from_str(&std::fs::read_to_string(path).expect("read yaml file"))
        .expect("parse yaml file")
}

// ============================================================================
// Provider roundtrip + credentials split
// ============================================================================

#[test]
fn provider_roundtrip_splits_api_key_into_credentials_and_removes_cleanly() {
    with_temp_dsh_dir(|dir| {
        dsh_config::set_provider(
            "demo",
            json!({
                "api": "openai-completions",
                "baseURL": "https://api.example.com/v1",
                "apiKey": "sk-live-secret",
                "models": [{ "id": "model-a", "name": "Model A" }],
            }),
        )
        .expect("set_provider");

        // settings.yaml 只落 apiKeyEnv 引用，密钥绝不出现
        let raw = std::fs::read_to_string(dir.join("settings.yaml")).expect("read settings.yaml");
        assert!(
            !raw.contains("sk-live-secret"),
            "secret must never land in settings.yaml:\n{raw}"
        );
        assert!(
            raw.contains("apiKeyEnv: DSH_DEMO_API_KEY"),
            "generated credential ref missing:\n{raw}"
        );

        // get_providers 反向物化 apiKey（并保留 apiKeyEnv 键）
        let providers = dsh_config::get_providers().expect("get_providers");
        let demo = providers.get("demo").expect("demo provider missing");
        assert_eq!(demo["apiKey"], "sk-live-secret");
        assert_eq!(demo["apiKeyEnv"], "DSH_DEMO_API_KEY");
        assert_eq!(demo["api"], "openai-completions");
        assert_eq!(demo["baseURL"], "https://api.example.com/v1");
        assert_eq!(demo["models"][0]["id"], "model-a");

        // credentials 文件：version: 1 + refs 条目
        let cred = read_yaml(&dir.join(".credentials.yaml"));
        assert_eq!(cred.get("version").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(
            cred.get("refs")
                .and_then(|v| v.get("DSH_DEMO_API_KEY"))
                .and_then(|v| v.as_str()),
            Some("sk-live-secret")
        );

        // remove_provider：provider 与其独占的 refs 条目一并清理
        dsh_config::remove_provider("demo").expect("remove_provider");
        assert!(
            dsh_config::get_providers()
                .expect("get_providers after remove")
                .is_empty(),
            "provider must be removed from settings.yaml"
        );
        let cred = read_yaml(&dir.join(".credentials.yaml"));
        assert!(
            cred.get("refs")
                .and_then(|v| v.get("DSH_DEMO_API_KEY"))
                .is_none(),
            "unreferenced credential ref must be removed"
        );
        assert_eq!(
            cred.get("version").and_then(|v| v.as_u64()),
            Some(1),
            "version: 1 must survive refs cleanup"
        );
    });
}

#[test]
fn credentials_write_preserves_version_records_and_existing_refs() {
    with_temp_dsh_dir(|dir| {
        std::fs::write(
            dir.join(".credentials.yaml"),
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
        .expect("seed credentials");

        dsh_config::set_provider(
            "demo",
            json!({
                "baseURL": "https://a.example.com",
                "apiKey": "sk-new",
                "models": [{ "id": "m" }],
            }),
        )
        .expect("set_provider");

        let cred = read_yaml(&dir.join(".credentials.yaml"));
        assert_eq!(cred.get("version").and_then(|v| v.as_u64()), Some(1));
        let refs = cred.get("refs").expect("refs section");
        assert_eq!(
            refs.get("EXISTING_KEY").and_then(|v| v.as_str()),
            Some("sk-old"),
            "pre-existing refs must be kept"
        );
        assert_eq!(
            refs.get("DSH_DEMO_API_KEY").and_then(|v| v.as_str()),
            Some("sk-new")
        );
        // records（OAuth/登录记录）原样保留
        assert_eq!(
            cred.get("records")
                .and_then(|v| v.get("llm-pi-ai/openai-codex"))
                .and_then(|v| v.get("kind"))
                .and_then(|v| v.as_str()),
            Some("grant"),
            "records section must be untouched"
        );
    });
}

// ============================================================================
// Default model switching
// ============================================================================

#[test]
fn set_default_model_switches_provider_and_preserves_reasoning_effort() {
    with_temp_dsh_dir(|dir| {
        std::fs::write(
            dir.join("settings.yaml"),
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
        )
        .expect("seed settings.yaml");

        dsh_config::set_provider(
            "new",
            json!({
                "baseURL": "https://new.example.com",
                "models": [{ "id": "new-model" }, { "id": "second-model" }],
            }),
        )
        .expect("set_provider");

        dsh_config::set_default_model("new").expect("set_default_model");

        let dm = dsh_config::get_default_model()
            .expect("get_default_model")
            .expect("default model section missing");
        assert_eq!(dm.provider, "new");
        // 模型取 models[0].id
        assert_eq!(dm.model, "new-model");
        // 已有的 reasoningEffort 原样保留
        assert_eq!(dm.reasoning_effort.as_deref(), Some("high"));

        let raw = std::fs::read_to_string(dir.join("settings.yaml")).expect("read settings.yaml");
        assert!(raw.contains("agent-default-model"));
    });
}

// ============================================================================
// MCP: cordis.patch.yml sync / remove / import
// ============================================================================

#[test]
fn sync_single_stdio_server_writes_insert_entry_and_is_idempotent() {
    with_temp_dsh_dir(|dir| {
        let spec = json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": { "GITHUB_TOKEN": "token" },
        });

        sync_single_server_to_dsh(&MultiAppConfig::default(), "github", &spec).expect("sync");

        let patch_path = dir.join("cordis.patch.yml");
        let first = std::fs::read_to_string(&patch_path).expect("read cordis.patch.yml");

        // 再 sync 一次：幂等，不产生重复条目、文件内容不变
        sync_single_server_to_dsh(&MultiAppConfig::default(), "github", &spec).expect("sync again");
        assert_eq!(
            first,
            std::fs::read_to_string(&patch_path).expect("read cordis.patch.yml again"),
            "second sync must be a no-op"
        );

        let yaml = read_yaml(&patch_path);
        let seq = yaml.as_sequence().expect("patch entries list");
        assert_eq!(seq.len(), 1, "no duplicate entries after re-sync");
        let insert = seq[0].get("insert").and_then(|v| v.as_sequence());
        let item = &insert.expect("insert list")[0];
        assert_eq!(item.get("id").and_then(|v| v.as_str()), Some("mcp-github"));
        assert_eq!(
            item.get("name").and_then(|v| v.as_str()),
            Some("@deepseek-ai/dsh-mcp-client")
        );
        let config = item.get("config").expect("item config");
        assert_eq!(
            config.get("serverName").and_then(|v| v.as_str()),
            Some("github")
        );
        assert_eq!(
            config.get("transport").and_then(|v| v.as_str()),
            Some("stdio")
        );
        assert_eq!(config.get("command").and_then(|v| v.as_str()), Some("npx"));
        assert_eq!(
            config
                .get("args")
                .and_then(|v| v.as_sequence())
                .map(|args| args.len()),
            Some(2)
        );
        assert_eq!(
            config
                .get("env")
                .and_then(|v| v.get("GITHUB_TOKEN"))
                .and_then(|v| v.as_str()),
            Some("token")
        );
    });
}

#[test]
fn sync_preserves_user_tagged_entry_and_remove_deletes_only_managed() {
    with_temp_dsh_dir(|dir| {
        // 预置一条带 !!js Tagged 值的"用户自有"条目（非 mcp- 前缀 id）
        let patch_path = dir.join("cordis.patch.yml");
        std::fs::write(
            &patch_path,
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
",
        )
        .expect("seed cordis.patch.yml");

        let servers: IndexMap<String, McpServer> = [make_dsh_server(
            "github",
            json!({ "type": "stdio", "command": "npx", "args": ["-y", "pkg"] }),
            true,
        )]
        .into_iter()
        .collect();
        sync_enabled_to_dsh(&servers).expect("sync_enabled_to_dsh");

        let yaml = read_yaml(&patch_path);
        let seq = yaml.as_sequence().expect("patch entries list");
        assert_eq!(seq.len(), 2, "user entry kept + managed entry added");

        // 用户自有条目逐字保留：!!js 标签行在文本层原样存在。
        // （serde_yaml 在 Value 层会丢标签，实现按文本块直传用户条目，
        // 不做 Value 往返，因此断言原文而非解析结果）
        let raw = std::fs::read_to_string(&patch_path).expect("read patch file");
        assert!(
            raw.contains("GITHUB_TOKEN: !!js process.env.GITHUB_TOKEN"),
            "!!js tagged line must survive verbatim, got:\n{raw}"
        );

        // remove：只删 cc-switch 管理的条目，用户自有条目原样保留
        remove_server_from_dsh("github").expect("remove_server_from_dsh");
        let yaml = read_yaml(&patch_path);
        let seq = yaml.as_sequence().expect("patch entries list");
        let remaining_ids: Vec<&str> = seq
            .iter()
            .filter_map(|entry| entry.get("insert").and_then(|v| v.as_sequence()))
            .flatten()
            .filter_map(|item| item.get("id").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(
            remaining_ids,
            vec!["my-manual"],
            "only the managed entry must be removed"
        );

        // 再确认 !!js 行在写回后仍逐字保留
        let raw = std::fs::read_to_string(&patch_path).expect("read patch file");
        assert!(raw.contains("GITHUB_TOKEN: !!js process.env.GITHUB_TOKEN"));
    });
}

#[test]
fn import_from_dsh_maps_stdio_and_streamable_http_servers() {
    with_temp_dsh_dir(|dir| {
        std::fs::write(
            dir.join("cordis.patch.yml"),
            "\
- insert:
    - id: mcp-github
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: github
        transport: stdio
        command: npx
        args: ['-y', 'pkg']
        env:
          KEY: value
- insert:
    - id: mcp-web
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: web
        transport: streamable-http
        url: https://example.com/mcp
        headers:
          Authorization: Bearer abc
",
        )
        .expect("seed cordis.patch.yml");

        let mut config = MultiAppConfig::default();
        let changed = import_from_dsh(&mut config).expect("import_from_dsh");
        assert_eq!(changed, 2, "both servers should be imported");

        let servers = config.mcp.servers.as_ref().expect("servers map");
        assert_eq!(servers.len(), 2);

        let github = servers.get("github").expect("github server");
        assert!(github.apps.dsh, "imported server enables dsh");
        assert!(
            !github.apps.claude && !github.apps.codex && !github.apps.gemini,
            "imported server defaults to dsh-only"
        );
        assert_eq!(github.server["type"], "stdio");
        assert_eq!(github.server["command"], "npx");
        assert_eq!(github.server["args"][0], "-y");
        assert_eq!(github.server["env"]["KEY"], "value");

        // streamable-http 映射回 http + url/headers
        let web = servers.get("web").expect("web server");
        assert_eq!(web.server["type"], "http");
        assert_eq!(web.server["url"], "https://example.com/mcp");
        assert_eq!(web.server["headers"]["Authorization"], "Bearer abc");
    });
}

// ============================================================================
// Common config snippets
// ============================================================================

#[test]
fn common_config_apply_applied_and_remove_roundtrip() {
    with_temp_dsh_dir(|_dir| {
        let snippet = "\
agent:
  max_turns: 10
ui:
  theme: dark
";
        assert!(
            !dsh_config::dsh_common_config_applied(snippet),
            "snippet must not be applied initially"
        );

        dsh_config::apply_dsh_common_config(snippet).expect("apply_dsh_common_config");
        assert!(
            dsh_config::dsh_common_config_applied(snippet),
            "snippet must be reported as applied after apply"
        );

        let settings = dsh_config::read_dsh_settings().expect("read settings");
        assert_eq!(
            settings
                .get("agent")
                .and_then(|v| v.get("max_turns"))
                .and_then(|v| v.as_u64()),
            Some(10)
        );
        assert_eq!(
            settings
                .get("ui")
                .and_then(|v| v.get("theme"))
                .and_then(|v| v.as_str()),
            Some("dark")
        );

        dsh_config::remove_dsh_common_config(snippet).expect("remove_dsh_common_config");
        assert!(
            !dsh_config::dsh_common_config_applied(snippet),
            "snippet must not be applied after remove"
        );
        let settings = dsh_config::read_dsh_settings().expect("read settings");
        assert!(settings.get("agent").is_none(), "applied key must be gone");
        assert!(settings.get("ui").is_none(), "applied key must be gone");
    });
}

// ============================================================================
// Validation
// ============================================================================

#[test]
fn validate_rejects_invalid_api_blank_base_url_and_empty_models() {
    let valid = json!({
        "api": "anthropic-messages",
        "baseURL": "https://a.example.com",
        "models": [{ "id": "m" }],
    });
    assert!(dsh_config::validate_dsh_provider_config(&valid).is_ok());

    let bad_api = json!({
        "api": "bedrock",
        "baseURL": "https://a.example.com",
        "models": [{ "id": "m" }],
    });
    assert!(
        dsh_config::validate_dsh_provider_config(&bad_api).is_err(),
        "api outside the three allowed protocols must be rejected"
    );

    let blank_base_url = json!({
        "baseURL": "  ",
        "models": [{ "id": "m" }],
    });
    assert!(
        dsh_config::validate_dsh_provider_config(&blank_base_url).is_err(),
        "blank baseURL must be rejected"
    );

    let empty_models = json!({
        "baseURL": "https://a.example.com",
        "models": [],
    });
    assert!(
        dsh_config::validate_dsh_provider_config(&empty_models).is_err(),
        "empty models array must be rejected"
    );

    let model_without_id = json!({
        "baseURL": "https://a.example.com",
        "models": [{ "name": "no-id" }],
    });
    assert!(
        dsh_config::validate_dsh_provider_config(&model_without_id).is_err(),
        "models entries must have a non-empty id"
    );
}
