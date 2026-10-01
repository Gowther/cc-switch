/**
 * Kimi Code CLI provider presets configuration
 * Kimi Code keeps providers in `[providers."<name>"]` and model aliases in
 * `[models."<alias>"]` of config.toml; each preset's settingsConfig follows the
 * flat snake_case shape consumed by the kimi-code backend:
 *
 * ```json
 * {
 *   "type": "anthropic",
 *   "base_url": "https://api.example.com",
 *   "api_key": "sk-...",
 *   "model": "claude-sonnet-5",
 *   "max_context_size": 1000000
 * }
 * ```
 */
import type { ProviderCategory } from "../types";
import type { PresetTheme, TemplateValueConfig } from "./claudeProviderPresets";

/** Kimi Code provider wire protocol (`type` in config.toml). Always written explicitly. */
export type KimiCodeProviderType =
  | "kimi"
  | "anthropic"
  | "openai"
  | "openai_responses"
  | "google-genai"
  | "vertexai";

/** Default provider type used when a provider has no stored value yet. */
export const KIMI_CODE_DEFAULT_TYPE: KimiCodeProviderType = "anthropic";

/** Dropdown options for the type selector. `labelKey` is looked up in i18n. */
export const kimiCodeProviderTypes: Array<{
  value: KimiCodeProviderType;
  labelKey: string;
}> = [
  { value: "kimi", labelKey: "kimiCode.form.typeKimi" },
  { value: "anthropic", labelKey: "kimiCode.form.typeAnthropic" },
  { value: "openai", labelKey: "kimiCode.form.typeOpenai" },
  { value: "openai_responses", labelKey: "kimiCode.form.typeOpenaiResponses" },
  { value: "google-genai", labelKey: "kimiCode.form.typeGoogleGenai" },
  { value: "vertexai", labelKey: "kimiCode.form.typeVertexai" },
];

export interface KimiCodeProviderSettingsConfig {
  type: KimiCodeProviderType;
  base_url: string;
  api_key: string;
  model: string;
  max_context_size?: number;
  display_name?: string;
  support_efforts?: string[];
  default_effort?: string;
  custom_headers?: Record<string, string>;
  [key: string]: unknown;
}

export interface KimiCodeProviderPreset {
  name: string;
  nameKey?: string;
  websiteUrl: string;
  apiKeyUrl?: string;
  settingsConfig: KimiCodeProviderSettingsConfig;
  isOfficial?: boolean;
  isPartner?: boolean;
  primePartner?: boolean; // 置顶合作伙伴（顶级）：徽章显示为心形
  partnerPromotionKey?: string;
  category?: ProviderCategory;
  templateValues?: Record<string, TemplateValueConfig>;
  theme?: PresetTheme;
  icon?: string;
  iconColor?: string;
  isCustomTemplate?: boolean;
}

export const kimiCodeProviderPresets: KimiCodeProviderPreset[] = [
  {
    name: "Kimi For Coding",
    websiteUrl: "https://www.kimi.com/code/?aff=cc-switch",
    apiKeyUrl: "https://platform.kimi.com/console/api-keys?aff=cc-switch",
    settingsConfig: {
      type: "kimi",
      base_url: "https://api.kimi.com/coding/v1",
      api_key: "",
      model: "kimi-for-coding",
      max_context_size: 262144,
      display_name: "Kimi For Coding",
    },
    isOfficial: true,
    category: "cn_official",
    icon: "kimi",
    iconColor: "#1783FF",
  },
  {
    name: "Kimi K3",
    websiteUrl: "https://platform.kimi.com?aff=cc-switch",
    apiKeyUrl: "https://platform.kimi.com/console/api-keys?aff=cc-switch",
    settingsConfig: {
      type: "kimi",
      base_url: "https://api.kimi.com/coding/v1",
      api_key: "",
      model: "k3",
      max_context_size: 1048576,
      display_name: "Kimi K3",
      support_efforts: ["low", "high", "max"],
      default_effort: "high",
    },
    isOfficial: true,
    category: "cn_official",
    icon: "kimi",
    iconColor: "#1783FF",
  },
  {
    name: "Zhipu GLM",
    websiteUrl: "https://open.bigmodel.cn",
    apiKeyUrl: "https://www.bigmodel.cn/claude-code?ic=RRVJPB5SII",
    settingsConfig: {
      type: "anthropic",
      base_url: "https://open.bigmodel.cn/api/anthropic",
      api_key: "",
      model: "glm-5.1",
      display_name: "GLM",
    },
    category: "cn_official",
    icon: "zhipu",
    iconColor: "#0F62FE",
  },
  {
    name: "DeepSeek",
    websiteUrl: "https://platform.deepseek.com",
    settingsConfig: {
      type: "openai",
      base_url: "https://api.deepseek.com",
      api_key: "",
      model: "deepseek-chat",
      display_name: "DeepSeek Chat",
    },
    category: "cn_official",
    icon: "deepseek",
    iconColor: "#4D6BFE",
  },
];
