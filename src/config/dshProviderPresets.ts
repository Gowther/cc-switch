/**
 * DSH (DeepSeek Harness) provider presets configuration
 * DSH uses an additive providers map; each entry's settingsConfig follows the
 * shape consumed by the dsh backend:
 *
 * ```json
 * {
 *   "api": "openai-completions",
 *   "baseURL": "https://api.deepseek.com",
 *   "apiKey": "sk-...",
 *   "models": [{ "id": "deepseek-chat" }],
 *   "compat": { "supportsDeveloperRole": false, "maxTokensField": "max_tokens" }
 * }
 * ```
 */
import type { ProviderCategory } from "../types";
import type { PresetTheme, TemplateValueConfig } from "./claudeProviderPresets";

/** DSH provider API protocol mode. Always written explicitly. */
export type DshApiMode =
  | "openai-completions"
  | "openai-responses"
  | "anthropic-messages";

/** Default mode used when a provider has no stored value yet. */
export const DSH_DEFAULT_API_MODE: DshApiMode = "openai-completions";

/** Dropdown options for the API mode selector. `labelKey` is looked up in i18n. */
export const dshApiModes: Array<{
  value: DshApiMode;
  labelKey: string;
}> = [
  { value: "openai-completions", labelKey: "dsh.form.apiOpenaiCompletions" },
  { value: "openai-responses", labelKey: "dsh.form.apiOpenaiResponses" },
  { value: "anthropic-messages", labelKey: "dsh.form.apiAnthropicMessages" },
];

/**
 * A model entry under a DSH provider. Only `id` is required; all other keys
 * (e.g. `input`, `reasoningEfforts`) are optional and passed through as-is.
 */
export interface DshModel {
  /** Model ID. */
  id: string;
  /** Supported input modalities, e.g. ["text", "image"]. */
  input?: string[];
  /** Reasoning effort mapping, e.g. { "high": "high" }. */
  reasoningEfforts?: Record<string, string>;
  [key: string]: unknown;
}

/** Optional compatibility overrides for a DSH provider. */
export interface DshCompat {
  supportsDeveloperRole?: boolean;
  maxTokensField?: string;
  [key: string]: unknown;
}

export interface DshProviderSettingsConfig {
  api: DshApiMode;
  baseURL: string;
  apiKey: string;
  models?: DshModel[];
  compat?: DshCompat;
  [key: string]: unknown;
}

export interface DshProviderPreset {
  name: string;
  nameKey?: string;
  websiteUrl: string;
  apiKeyUrl?: string;
  settingsConfig: DshProviderSettingsConfig;
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

export const dshProviderPresets: DshProviderPreset[] = [
  {
    name: "DeepSeek",
    nameKey: "providerForm.presets.deepseek",
    websiteUrl: "https://platform.deepseek.com",
    apiKeyUrl: "https://platform.deepseek.com/api_keys",
    settingsConfig: {
      api: "openai-completions",
      baseURL: "https://api.deepseek.com",
      apiKey: "",
      models: [{ id: "deepseek-chat" }, { id: "deepseek-reasoner" }],
    },
    category: "cn_official",
    icon: "deepseek",
    iconColor: "#4D6BFE",
  },
  {
    name: "Kimi",
    primePartner: true,
    websiteUrl: "https://platform.kimi.com?aff=cc-switch",
    apiKeyUrl: "https://platform.kimi.com/console/api-keys?aff=cc-switch",
    settingsConfig: {
      api: "anthropic-messages",
      baseURL: "https://api.moonshot.cn/anthropic",
      apiKey: "",
      models: [{ id: "kimi-k2.7-code" }],
    },
    category: "cn_official",
    icon: "kimi",
    iconColor: "#6366F1",
  },
  {
    name: "Zhipu GLM en",
    websiteUrl: "https://z.ai",
    apiKeyUrl: "https://z.ai/subscribe?ic=8JVLJQFSKB",
    settingsConfig: {
      api: "anthropic-messages",
      baseURL: "https://api.z.ai/api/anthropic",
      apiKey: "",
      models: [{ id: "glm-5.1" }],
    },
    category: "cn_official",
    icon: "zhipu",
    iconColor: "#0F62FE",
  },
];
