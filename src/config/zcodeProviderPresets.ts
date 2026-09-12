/**
 * ZCode (智谱 z.ai Agentic IDE) provider presets configuration
 * ZCode uses an additive providers map; each entry's settingsConfig follows the
 * shape consumed by the zcode backend:
 *
 * ```json
 * {
 *   "kind": "anthropic",
 *   "baseURL": "https://api.z.ai/api/anthropic",
 *   "apiKey": "sk-...",
 *   "models": [{ "id": "glm-5.1", "name": "GLM-5.1" }]
 * }
 * ```
 */
import type { ProviderCategory } from "../types";
import type { PresetTheme, TemplateValueConfig } from "./claudeProviderPresets";

/** ZCode provider API protocol kind. Always written explicitly. */
export type ZcodeKind = "anthropic" | "openai" | "openai-compatible";

/** Default kind used when a provider has no stored value yet. */
export const ZCODE_DEFAULT_KIND: ZcodeKind = "anthropic";

/** Dropdown options for the kind selector. `labelKey` is looked up in i18n. */
export const zcodeKinds: Array<{
  value: ZcodeKind;
  labelKey: string;
}> = [
  { value: "anthropic", labelKey: "zcode.form.kindAnthropic" },
  { value: "openai", labelKey: "zcode.form.kindOpenai" },
  { value: "openai-compatible", labelKey: "zcode.form.kindOpenaiCompatible" },
];

/**
 * A model entry under a ZCode provider. Only `id` is required; all other keys
 * (e.g. `name`, `reasoning`, `limit`) are optional and passed through as-is.
 */
export interface ZcodeModel {
  /** Model ID. */
  id: string;
  /** Display name. */
  name?: string;
  /** Whether the model supports reasoning. */
  reasoning?: boolean;
  /** Context/output token limits. */
  limit?: {
    context?: number;
    output?: number;
    [key: string]: unknown;
  };
  [key: string]: unknown;
}

export interface ZcodeProviderSettingsConfig {
  kind: ZcodeKind;
  baseURL: string;
  apiKey: string;
  apiKeyRequired?: boolean;
  headers?: Record<string, string>;
  models?: ZcodeModel[];
  [key: string]: unknown;
}

export interface ZcodeProviderPreset {
  name: string;
  nameKey?: string;
  websiteUrl: string;
  apiKeyUrl?: string;
  settingsConfig: ZcodeProviderSettingsConfig;
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

export const zcodeProviderPresets: ZcodeProviderPreset[] = [
  {
    name: "Zhipu GLM en",
    websiteUrl: "https://z.ai",
    apiKeyUrl: "https://z.ai/subscribe?ic=8JVLJQFSKB",
    settingsConfig: {
      kind: "anthropic",
      baseURL: "https://api.z.ai/api/anthropic",
      apiKey: "",
      models: [{ id: "glm-5.1", name: "GLM-5.1" }],
    },
    isOfficial: true,
    category: "cn_official",
    icon: "zhipu",
    iconColor: "#0F62FE",
  },
  {
    name: "Zhipu GLM",
    websiteUrl: "https://open.bigmodel.cn",
    apiKeyUrl: "https://www.bigmodel.cn/claude-code?ic=RRVJPB5SII",
    settingsConfig: {
      kind: "anthropic",
      baseURL: "https://open.bigmodel.cn/api/anthropic",
      apiKey: "",
      models: [{ id: "glm-5.1", name: "GLM-5.1" }],
    },
    category: "cn_official",
    icon: "zhipu",
    iconColor: "#0F62FE",
  },
  {
    name: "Kimi",
    websiteUrl: "https://platform.kimi.com?aff=cc-switch",
    apiKeyUrl: "https://platform.kimi.com/console/api-keys?aff=cc-switch",
    settingsConfig: {
      kind: "openai-compatible",
      baseURL: "https://api.moonshot.cn/v1",
      apiKey: "",
      models: [{ id: "kimi-k2.7-code", name: "Kimi K2.7 Code" }],
    },
    category: "cn_official",
    icon: "kimi",
    iconColor: "#6366F1",
  },
];
