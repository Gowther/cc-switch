import { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import { configApi } from "@/lib/api";
import {
  hasCommonConfigSnippet,
  updateCommonConfigSnippet,
} from "@/utils/providerConfigUtils";

const LEGACY_STORAGE_KEY = "cc-switch:gemini-common-config-snippet";
const DEFAULT_GEMINI_COMMON_CONFIG_SNIPPET = "{}";

const GEMINI_SENSITIVE_ENV_EXACT_KEYS = new Set([
  "APIKEY",
  "API_KEY",
  "TOKEN",
  "SECRET",
  "PASSWORD",
  "CREDENTIALS",
]);
const GEMINI_SENSITIVE_ENV_SUFFIXES = [
  "_API_KEY",
  "_APIKEY",
  "_AUTH_TOKEN",
  "_TOKEN",
  "_ACCESS_KEY",
  "_ACCESS_KEY_ID",
  "_KEY_ID",
  "_PRIVATE_KEY",
];
const GEMINI_SENSITIVE_ENV_PARTS = [
  "SECRET",
  "PASSWORD",
  "PASSWD",
  "CREDENTIAL",
  "PRIVATE_KEY",
  "BEARER_TOKEN",
];

function isGeminiSensitiveEnvKey(key: string): boolean {
  const upper = key.toUpperCase();

  return (
    upper === "GOOGLE_GEMINI_BASE_URL" ||
    GEMINI_SENSITIVE_ENV_EXACT_KEYS.has(upper) ||
    GEMINI_SENSITIVE_ENV_SUFFIXES.some((suffix) => upper.endsWith(suffix)) ||
    GEMINI_SENSITIVE_ENV_PARTS.some((part) => upper.includes(part))
  );
}

interface UseGeminiCommonConfigProps {
  envValue: string;
  onEnvChange: (env: string) => void;
  configValue: string;
  onConfigChange: (config: string) => void;
  envStringToObj: (envString: string) => Record<string, string>;
  envObjToString: (envObj: Record<string, unknown>) => string;
  initialData?: {
    settingsConfig?: Record<string, unknown>;
  };
  initialEnabled?: boolean;
  selectedPresetId?: string;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    Object.prototype.toString.call(value) === "[object Object]"
  );
}

/**
 * 管理 Gemini 通用配置片段 (JSON 格式)
 * 同时共享 `.env` 和 settings.json，但会排除供应商端点和凭据字段。
 */
export function useGeminiCommonConfig({
  envValue,
  onEnvChange,
  configValue,
  onConfigChange,
  envStringToObj,
  envObjToString,
  initialData,
  initialEnabled,
  selectedPresetId,
}: UseGeminiCommonConfigProps) {
  const { t } = useTranslation();
  const [useCommonConfig, setUseCommonConfig] = useState(false);
  const [commonConfigSnippet, setCommonConfigSnippetState] = useState<string>(
    DEFAULT_GEMINI_COMMON_CONFIG_SNIPPET,
  );
  const [commonConfigError, setCommonConfigError] = useState("");
  const [isLoading, setIsLoading] = useState(true);
  const [isExtracting, setIsExtracting] = useState(false);

  // 用于跟踪是否正在通过通用配置更新
  const isUpdatingFromCommonConfig = useRef(false);
  // 用于跟踪新建模式是否已初始化默认勾选
  const hasInitializedNewMode = useRef(false);
  // 用于跟踪编辑模式是否已初始化显式开关/预览
  const hasInitializedEditMode = useRef(false);

  // 当预设变化时，重置初始化标记，使新预设能够重新触发初始化逻辑
  useEffect(() => {
    hasInitializedNewMode.current = false;
    hasInitializedEditMode.current = false;
  }, [selectedPresetId, initialEnabled]);

  const parseSnippet = useCallback(
    (
      snippetString: string,
    ): {
      env: Record<string, string>;
      config: Record<string, unknown>;
      normalized: Record<string, unknown>;
      error?: string;
    } => {
      const trimmed = snippetString.trim();
      if (!trimmed) {
        return { env: {}, config: {}, normalized: {} };
      }

      let parsed: unknown;
      try {
        parsed = JSON.parse(trimmed);
      } catch {
        return {
          env: {},
          config: {},
          normalized: {},
          error: t("geminiConfig.invalidJsonFormat"),
        };
      }

      if (!isPlainObject(parsed)) {
        return {
          env: {},
          config: {},
          normalized: {},
          error: t("geminiConfig.invalidJsonFormat"),
        };
      }

      const structured = "env" in parsed || "config" in parsed;
      const envSource = structured ? (parsed.env ?? {}) : parsed;
      const configSource = structured ? (parsed.config ?? {}) : {};
      if (!isPlainObject(envSource) || !isPlainObject(configSource)) {
        return {
          env: {},
          config: {},
          normalized: {},
          error: t("geminiConfig.invalidJsonFormat"),
        };
      }
      if (
        structured &&
        Object.keys(parsed).some((key) => key !== "env" && key !== "config")
      ) {
        return {
          env: {},
          config: {},
          normalized: {},
          error: t("geminiConfig.invalidJsonFormat"),
        };
      }

      const forbiddenKeys = Object.keys(envSource).filter(
        isGeminiSensitiveEnvKey,
      );
      if (forbiddenKeys.length > 0) {
        return {
          env: {},
          config: {},
          normalized: {},
          error: t("geminiConfig.commonConfigInvalidKeys", {
            keys: forbiddenKeys.join(", "),
          }),
        };
      }

      const env: Record<string, string> = {};
      for (const [key, value] of Object.entries(envSource)) {
        if (typeof value !== "string") {
          return {
            env: {},
            config: {},
            normalized: {},
            error: t("geminiConfig.commonConfigInvalidValues"),
          };
        }
        const normalized = value.trim();
        if (!normalized) continue;
        env[key] = normalized;
      }

      const config = configSource as Record<string, unknown>;
      const normalized: Record<string, unknown> = {};
      if (Object.keys(env).length > 0) normalized.env = env;
      if (Object.keys(config).length > 0) normalized.config = config;

      return { env, config, normalized };
    },
    [t],
  );

  const getCurrentSettings = useCallback(() => {
    let config: unknown = {};
    try {
      config = configValue.trim() ? JSON.parse(configValue) : {};
    } catch {
      return { error: t("geminiConfig.invalidJsonFormat") };
    }
    if (!isPlainObject(config)) {
      return { error: t("geminiConfig.invalidJsonFormat") };
    }
    return {
      settings: {
        env: envStringToObj(envValue),
        config,
      },
    };
  }, [configValue, envStringToObj, envValue, t]);

  const commitSettings = useCallback(
    (settings: Record<string, unknown>) => {
      const env = isPlainObject(settings.env) ? settings.env : {};
      const config = isPlainObject(settings.config) ? settings.config : {};
      onEnvChange(envObjToString(env));
      onConfigChange(JSON.stringify(config, null, 2));
    },
    [envObjToString, onConfigChange, onEnvChange],
  );

  const updateCurrentSettings = useCallback(
    (snippet: Record<string, unknown>, enabled: boolean) => {
      const current = getCurrentSettings();
      if (!current.settings) {
        return { error: current.error };
      }
      const result = updateCommonConfigSnippet(
        JSON.stringify(current.settings),
        JSON.stringify(snippet),
        enabled,
      );
      if (result.error) return { error: result.error };

      commitSettings(JSON.parse(result.updatedConfig));
      return {};
    },
    [commitSettings, getCurrentSettings],
  );

  // 初始化：从 config.json 加载，支持从 localStorage 迁移
  useEffect(() => {
    let mounted = true;

    const loadSnippet = async () => {
      try {
        // 使用统一 API 加载
        const snippet = await configApi.getCommonConfigSnippet("gemini");

        if (snippet && snippet.trim()) {
          if (mounted) {
            setCommonConfigSnippetState(snippet);
          }
        } else {
          // 如果 config.json 中没有，尝试从 localStorage 迁移
          if (typeof window !== "undefined") {
            try {
              const legacySnippet =
                window.localStorage.getItem(LEGACY_STORAGE_KEY);
              if (legacySnippet && legacySnippet.trim()) {
                const parsed = parseSnippet(legacySnippet);
                if (parsed.error) {
                  console.warn(
                    "[迁移] legacy Gemini 通用配置片段格式不符合当前规则，跳过迁移",
                  );
                  return;
                }
                // 迁移到 config.json
                await configApi.setCommonConfigSnippet("gemini", legacySnippet);
                if (mounted) {
                  setCommonConfigSnippetState(legacySnippet);
                }
                // 清理 localStorage
                window.localStorage.removeItem(LEGACY_STORAGE_KEY);
                console.log(
                  "[迁移] Gemini 通用配置已从 localStorage 迁移到 config.json",
                );
              }
            } catch (e) {
              console.warn("[迁移] 从 localStorage 迁移失败:", e);
            }
          }
        }
      } catch (error) {
        console.error("加载 Gemini 通用配置失败:", error);
      } finally {
        if (mounted) {
          setIsLoading(false);
        }
      }
    };

    loadSnippet();

    return () => {
      mounted = false;
    };
  }, [parseSnippet]);

  // 初始化时检查通用配置片段（编辑模式）
  useEffect(() => {
    if (
      !initialData?.settingsConfig ||
      isLoading ||
      hasInitializedEditMode.current
    ) {
      return;
    }

    hasInitializedEditMode.current = true;

    const parsed = parseSnippet(commonConfigSnippet);
    if (parsed.error) {
      if (commonConfigSnippet.trim()) {
        setCommonConfigError(parsed.error);
      }
      setUseCommonConfig(false);
      return;
    }

    const hasContent = Object.keys(parsed.normalized).length > 0;
    const inferredHasCommon =
      hasContent &&
      hasCommonConfigSnippet(
        JSON.stringify(initialData.settingsConfig),
        JSON.stringify(parsed.normalized),
      );
    const hasCommon =
      initialEnabled !== undefined ? initialEnabled : inferredHasCommon;

    if (hasCommon && !inferredHasCommon && hasContent) {
      isUpdatingFromCommonConfig.current = true;
      const result = updateCurrentSettings(parsed.normalized, true);
      if (result.error) {
        isUpdatingFromCommonConfig.current = false;
        setCommonConfigError(result.error);
        setUseCommonConfig(false);
        return;
      }
      setTimeout(() => {
        isUpdatingFromCommonConfig.current = false;
      }, 0);
    }

    setCommonConfigError("");
    setUseCommonConfig(hasCommon);
  }, [
    commonConfigSnippet,
    initialData,
    initialEnabled,
    isLoading,
    parseSnippet,
    updateCurrentSettings,
  ]);

  // 新建模式：如果通用配置片段存在且有效，默认启用
  useEffect(() => {
    if (initialData || isLoading || hasInitializedNewMode.current) {
      return;
    }

    hasInitializedNewMode.current = true;

    const parsed = parseSnippet(commonConfigSnippet);
    if (parsed.error) {
      if (commonConfigSnippet.trim()) {
        setCommonConfigError(parsed.error);
      }
      setUseCommonConfig(false);
      return;
    }
    const hasContent = Object.keys(parsed.normalized).length > 0;
    if (!hasContent) return;

    isUpdatingFromCommonConfig.current = true;
    const result = updateCurrentSettings(parsed.normalized, true);
    if (result.error) {
      isUpdatingFromCommonConfig.current = false;
      setCommonConfigError(result.error);
      setUseCommonConfig(false);
      return;
    }
    setCommonConfigError("");
    setUseCommonConfig(true);
    setTimeout(() => {
      isUpdatingFromCommonConfig.current = false;
    }, 0);
  }, [
    initialData,
    isLoading,
    commonConfigSnippet,
    parseSnippet,
    updateCurrentSettings,
  ]);

  // 处理通用配置开关
  const handleCommonConfigToggle = useCallback(
    (checked: boolean, snippet = commonConfigSnippet) => {
      const parsed = parseSnippet(snippet);
      if (parsed.error) {
        setCommonConfigError(parsed.error);
        setUseCommonConfig(false);
        return;
      }
      if (Object.keys(parsed.normalized).length === 0) {
        setCommonConfigError(t("geminiConfig.noCommonConfigToApply"));
        setUseCommonConfig(false);
        return;
      }

      isUpdatingFromCommonConfig.current = true;
      const result = updateCurrentSettings(parsed.normalized, checked);
      if (result.error) {
        isUpdatingFromCommonConfig.current = false;
        setCommonConfigError(result.error);
        setUseCommonConfig(false);
        return;
      }
      setCommonConfigError("");
      setUseCommonConfig(checked);
      setTimeout(() => {
        isUpdatingFromCommonConfig.current = false;
      }, 0);
    },
    [commonConfigSnippet, parseSnippet, t, updateCurrentSettings],
  );

  // 处理通用配置片段变化
  const handleCommonConfigSnippetChange = useCallback(
    (value: string): boolean => {
      const previousSnippet = commonConfigSnippet;

      if (!value.trim()) {
        setCommonConfigError("");

        if (useCommonConfig) {
          const parsedPrevious = parseSnippet(previousSnippet);
          if (!parsedPrevious.error) {
            isUpdatingFromCommonConfig.current = true;
            const result = updateCurrentSettings(
              parsedPrevious.normalized,
              false,
            );
            if (result.error) {
              isUpdatingFromCommonConfig.current = false;
              setCommonConfigError(result.error);
              return false;
            }
            setTimeout(() => {
              isUpdatingFromCommonConfig.current = false;
            }, 0);
          }
          setUseCommonConfig(false);
        }

        setCommonConfigSnippetState("");
        configApi
          .setCommonConfigSnippet("gemini", "")
          .catch((error: unknown) => {
            console.error("保存 Gemini 通用配置失败:", error);
            setCommonConfigError(
              t("geminiConfig.saveFailed", { error: String(error) }),
            );
          });
        return true;
      }

      // 校验 JSON 格式
      const parsed = parseSnippet(value);
      if (parsed.error) {
        setCommonConfigError(parsed.error);
        return false;
      }

      // 若当前启用通用配置，需要替换为最新片段
      if (useCommonConfig) {
        const current = getCurrentSettings();
        if (!current.settings) {
          setCommonConfigError(current.error ?? "Invalid Gemini config");
          return false;
        }

        let nextSettings = JSON.stringify(current.settings);
        const prevParsed = parseSnippet(previousSnippet);
        if (
          !prevParsed.error &&
          Object.keys(prevParsed.normalized).length > 0
        ) {
          const removed = updateCommonConfigSnippet(
            nextSettings,
            JSON.stringify(prevParsed.normalized),
            false,
          );
          if (removed.error) {
            setCommonConfigError(removed.error);
            return false;
          }
          nextSettings = removed.updatedConfig;
        }
        if (Object.keys(parsed.normalized).length > 0) {
          const added = updateCommonConfigSnippet(
            nextSettings,
            JSON.stringify(parsed.normalized),
            true,
          );
          if (added.error) {
            setCommonConfigError(added.error);
            return false;
          }
          nextSettings = added.updatedConfig;
        }

        isUpdatingFromCommonConfig.current = true;
        commitSettings(JSON.parse(nextSettings));
        setTimeout(() => {
          isUpdatingFromCommonConfig.current = false;
        }, 0);
      }

      setCommonConfigError("");
      setCommonConfigSnippetState(value);
      configApi
        .setCommonConfigSnippet("gemini", value)
        .catch((error: unknown) => {
          console.error("保存 Gemini 通用配置失败:", error);
          setCommonConfigError(
            t("geminiConfig.saveFailed", { error: String(error) }),
          );
        });

      return true;
    },
    [
      commitSettings,
      commonConfigSnippet,
      getCurrentSettings,
      parseSnippet,
      t,
      updateCurrentSettings,
      useCommonConfig,
    ],
  );

  // 仅 legacy Provider 没有显式开关时，才从编辑内容推断一次状态。
  useEffect(() => {
    if (
      isUpdatingFromCommonConfig.current ||
      isLoading ||
      initialEnabled !== undefined
    ) {
      return;
    }
    const parsed = parseSnippet(commonConfigSnippet);
    if (parsed.error || Object.keys(parsed.normalized).length === 0) return;
    const current = getCurrentSettings();
    if (!current.settings) return;
    setUseCommonConfig(
      hasCommonConfigSnippet(
        JSON.stringify(current.settings),
        JSON.stringify(parsed.normalized),
      ),
    );
  }, [
    commonConfigSnippet,
    getCurrentSettings,
    initialEnabled,
    isLoading,
    parseSnippet,
  ]);

  // 从编辑器当前内容提取通用配置片段
  const handleExtract = useCallback(async () => {
    setIsExtracting(true);
    setCommonConfigError("");

    try {
      const extracted = await configApi.extractCommonConfigSnippet("gemini", {
        settingsConfig: JSON.stringify({
          env: envStringToObj(envValue),
          config: configValue.trim() ? JSON.parse(configValue) : {},
        }),
      });

      if (!extracted || extracted === "{}") {
        setCommonConfigError(t("geminiConfig.extractNoCommonConfig"));
        return;
      }

      // 验证 JSON 格式
      const parsed = parseSnippet(extracted);
      if (parsed.error) {
        setCommonConfigError(t("geminiConfig.extractedConfigInvalid"));
        return;
      }

      // 更新片段状态
      setCommonConfigSnippetState(extracted);

      // 保存到后端
      await configApi.setCommonConfigSnippet("gemini", extracted);
    } catch (error) {
      console.error("提取 Gemini 通用配置失败:", error);
      setCommonConfigError(
        t("geminiConfig.extractFailed", { error: String(error) }),
      );
    } finally {
      setIsExtracting(false);
    }
  }, [configValue, envStringToObj, envValue, parseSnippet, t]);

  const clearCommonConfigError = useCallback(() => {
    setCommonConfigError("");
  }, []);

  return {
    useCommonConfig,
    commonConfigSnippet,
    commonConfigError,
    isLoading,
    isExtracting,
    handleCommonConfigToggle,
    handleCommonConfigSnippetChange,
    handleExtract,
    clearCommonConfigError,
  };
}
