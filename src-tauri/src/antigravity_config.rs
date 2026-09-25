//! Antigravity（Google）配置文件读写模块
//!
//! Antigravity 产品族（2.0 / IDE / CLI）与 Gemini CLI 共享 `~/.gemini`，
//! 其共享 MCP / Skills 配置位于 `~/.gemini/config/`：
//!
//! - `mcp_config.json` —— MCP 服务器（`{"mcpServers": {...}}`；stdio 用
//!   `command`/`args`/`env`/`cwd`，http/sse 用 `serverUrl`/`headers`，停用
//!   键为 `disabled: true`）
//! - `skills/<name>/SKILL.md` —— Skills 目录
//!
//! Antigravity 官方不支持自定义模型供应商（无 BYOK），因此本模块只承载
//! 路径与 MCP 文件读写，无供应商管理。
//!
//! 注意：Antigravity GUI 不是热监听这些文件（MCP 面板需手动 Refresh），
//! 且 GUI 自身会回写——外部写入的生效时机需要用户刷新或重启。

use crate::config::atomic_write;
use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

// ============================================================================
// Backup
// ============================================================================

/// 备份到 `<cc-switch 配置目录>/backups/antigravity/antigravity_mcp_*.json`，
/// 策略对齐 dsh_config：时间戳命名 + 保留数量清理。
fn create_antigravity_backup(source: &str) -> Result<PathBuf, AppError> {
    use crate::config::get_app_config_dir;
    use crate::settings::effective_backup_retain_count;
    use chrono::Local;

    let backup_dir = get_app_config_dir().join("backups").join("antigravity");
    fs::create_dir_all(&backup_dir).map_err(|e| AppError::io(&backup_dir, e))?;

    let base_id = format!("antigravity_mcp_{}", Local::now().format("%Y%m%d_%H%M%S"));
    let mut filename = format!("{base_id}.json");
    let mut backup_path = backup_dir.join(&filename);
    let mut counter = 1;
    while backup_path.exists() {
        filename = format!("{base_id}_{counter}.json");
        backup_path = backup_dir.join(&filename);
        counter += 1;
    }

    atomic_write(&backup_path, source.as_bytes())?;

    // 清理旧备份
    let retain = effective_backup_retain_count();
    let mut entries = fs::read_dir(&backup_dir)
        .map_err(|e| AppError::io(&backup_dir, e))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("antigravity_mcp_")
        })
        .collect::<Vec<_>>();
    if entries.len() > retain {
        entries.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
        for entry in entries.into_iter().take(entries.len() - retain) {
            let _ = fs::remove_file(entry.path());
        }
    }

    Ok(backup_path)
}

// ============================================================================
// Path Functions
// ============================================================================

/// Antigravity MCP 配置文件路径（`<dir>/mcp_config.json`）
pub fn get_antigravity_mcp_path() -> PathBuf {
    crate::settings::get_antigravity_dir().join("mcp_config.json")
}

/// Antigravity Skills 目录（`<dir>/skills`）
pub fn get_antigravity_skills_dir() -> PathBuf {
    crate::settings::get_antigravity_dir().join("skills")
}

fn antigravity_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ============================================================================
// mcp_config.json 读-改-写
// ============================================================================

/// 读取 mcp_config.json 为 JSON 对象（缺失/空/非对象 → 空对象）。
pub fn read_antigravity_mcp_config() -> Result<serde_json::Map<String, serde_json::Value>, AppError>
{
    let path = get_antigravity_mcp_path();
    if !path.exists() {
        return Ok(serde_json::Map::new());
    }
    let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    if content.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    let value: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        AppError::Config(format!("Failed to parse Antigravity mcp_config.json: {e}"))
    })?;
    Ok(value.as_object().cloned().unwrap_or_default())
}

/// mcp_config.json 的读-改-写入口：写锁内执行 updater，写前备份 + 原子写。
pub(crate) fn update_antigravity_mcp_config(
    updater: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let _guard = antigravity_write_lock().lock()?;
    let path = get_antigravity_mcp_path();

    let mut root = read_antigravity_mcp_config()?;
    updater(&mut root)?;

    let serialized = serde_json::to_string_pretty(&serde_json::Value::Object(root))
        .map_err(|e| AppError::Config(e.to_string()))?
        + "\n";

    if path.exists() {
        let existing = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
        if existing == serialized {
            return Ok(());
        }
        create_antigravity_backup(&existing)?;
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }
    atomic_write(&path, serialized.as_bytes())?;
    Ok(())
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

    #[test]
    #[serial]
    fn mcp_config_roundtrip_preserves_other_keys() {
        with_test_home(|| {
            let path = get_antigravity_mcp_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"other": true, "mcpServers": {}}"#).unwrap();

            update_antigravity_mcp_config(|root| {
                root.entry("mcpServers".to_string())
                    .or_insert_with(|| serde_json::json!({}))
                    .as_object_mut()
                    .unwrap()
                    .insert("fs".to_string(), serde_json::json!({"command": "npx"}));
                Ok(())
            })
            .unwrap();

            let root = read_antigravity_mcp_config().unwrap();
            assert_eq!(
                root.get("other").and_then(|v| v.as_bool()),
                Some(true),
                "其他顶层键保留"
            );
            assert!(root.get("mcpServers").and_then(|v| v.get("fs")).is_some());
        });
    }

    #[test]
    #[serial]
    fn read_missing_file_returns_empty() {
        with_test_home(|| {
            assert!(read_antigravity_mcp_config().unwrap().is_empty());
        });
    }
}
