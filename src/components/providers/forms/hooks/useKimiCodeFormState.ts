import { useState, useCallback, useMemo } from "react";
import type { AppId } from "@/lib/api";
import { useProvidersQuery } from "@/lib/query/queries";
import {
  KIMI_CODE_DEFAULT_TYPE,
  type KimiCodeProviderType,
  type KimiCodeProviderSettingsConfig,
} from "@/config/kimiCodeProviderPresets";

interface UseKimiCodeFormStateParams {
  initialData?: {
    settingsConfig?: Record<string, unknown>;
  };
  appId: AppId;
  providerId?: string;
  onSettingsConfigChange: (config: string) => void;
  getSettingsConfig: () => string;
}

const KIMI_CODE_DEFAULT_CONFIG_OBJ = {
  type: KIMI_CODE_DEFAULT_TYPE,
  base_url: "",
  api_key: "",
  model: "",
} as const;

export const KIMI_CODE_DEFAULT_CONFIG = JSON.stringify(
  KIMI_CODE_DEFAULT_CONFIG_OBJ,
  null,
  2,
);

export interface KimiCodeFormState {
  kimiCodeProviderKey: string;
  setKimiCodeProviderKey: (key: string) => void;
  kimiCodeBaseUrl: string;
  kimiCodeApiKey: string;
  kimiCodeModel: string;
  kimiCodeType: KimiCodeProviderType;
  existingKimiCodeKeys: string[];
  handleKimiCodeBaseUrlChange: (baseUrl: string) => void;
  handleKimiCodeApiKeyChange: (apiKey: string) => void;
  handleKimiCodeModelChange: (model: string) => void;
  handleKimiCodeTypeChange: (type: KimiCodeProviderType) => void;
  resetKimiCodeState: (
    config?: Partial<KimiCodeProviderSettingsConfig>,
  ) => void;
}

function parseKimiCodeField<T>(
  initialData: UseKimiCodeFormStateParams["initialData"],
  field: string,
  fallback: T,
): T {
  try {
    if (initialData?.settingsConfig) {
      return (initialData.settingsConfig[field] as T) || fallback;
    }
    return (
      ((KIMI_CODE_DEFAULT_CONFIG_OBJ as Record<string, unknown>)[field] as T) ||
      fallback
    );
  } catch {
    return fallback;
  }
}

export function useKimiCodeFormState({
  initialData,
  appId,
  providerId,
  onSettingsConfigChange,
  getSettingsConfig,
}: UseKimiCodeFormStateParams): KimiCodeFormState {
  const { data: kimiCodeProvidersData } = useProvidersQuery("kimi-code");
  const existingKimiCodeKeys = useMemo(() => {
    if (!kimiCodeProvidersData?.providers) return [];
    return Object.keys(kimiCodeProvidersData.providers).filter(
      (k) => k !== providerId,
    );
  }, [kimiCodeProvidersData?.providers, providerId]);

  const [kimiCodeProviderKey, setKimiCodeProviderKey] = useState<string>(() => {
    if (appId !== "kimi-code") return "";
    return providerId || "";
  });

  const [kimiCodeBaseUrl, setKimiCodeBaseUrl] = useState<string>(() => {
    if (appId !== "kimi-code") return "";
    return parseKimiCodeField(initialData, "base_url", "");
  });

  const [kimiCodeApiKey, setKimiCodeApiKey] = useState<string>(() => {
    if (appId !== "kimi-code") return "";
    return parseKimiCodeField(initialData, "api_key", "");
  });

  const [kimiCodeModel, setKimiCodeModel] = useState<string>(() => {
    if (appId !== "kimi-code") return "";
    return parseKimiCodeField(initialData, "model", "");
  });

  const [kimiCodeType, setKimiCodeType] = useState<KimiCodeProviderType>(() => {
    if (appId !== "kimi-code") return KIMI_CODE_DEFAULT_TYPE;
    const stored = parseKimiCodeField<KimiCodeProviderType | "">(
      initialData,
      "type",
      "",
    );
    return stored || KIMI_CODE_DEFAULT_TYPE;
  });

  const updateKimiCodeConfig = useCallback(
    (updater: (config: Record<string, unknown>) => void) => {
      try {
        const config = JSON.parse(
          getSettingsConfig() || KIMI_CODE_DEFAULT_CONFIG,
        );
        updater(config);
        onSettingsConfigChange(JSON.stringify(config, null, 2));
      } catch {
        // ignore
      }
    },
    [getSettingsConfig, onSettingsConfigChange],
  );

  const handleKimiCodeBaseUrlChange = useCallback(
    (baseUrl: string) => {
      setKimiCodeBaseUrl(baseUrl);
      updateKimiCodeConfig((config) => {
        config.base_url = baseUrl.trim().replace(/\/+$/, "");
      });
    },
    [updateKimiCodeConfig],
  );

  const handleKimiCodeApiKeyChange = useCallback(
    (apiKey: string) => {
      setKimiCodeApiKey(apiKey);
      updateKimiCodeConfig((config) => {
        config.api_key = apiKey;
      });
    },
    [updateKimiCodeConfig],
  );

  const handleKimiCodeModelChange = useCallback(
    (model: string) => {
      setKimiCodeModel(model);
      updateKimiCodeConfig((config) => {
        config.model = model.trim();
      });
    },
    [updateKimiCodeConfig],
  );

  const handleKimiCodeTypeChange = useCallback(
    (type: KimiCodeProviderType) => {
      setKimiCodeType(type);
      updateKimiCodeConfig((config) => {
        config.type = type;
      });
    },
    [updateKimiCodeConfig],
  );

  const resetKimiCodeState = useCallback(
    (config?: Partial<KimiCodeProviderSettingsConfig>) => {
      setKimiCodeProviderKey("");
      setKimiCodeBaseUrl(config?.base_url || "");
      setKimiCodeApiKey(config?.api_key || "");
      setKimiCodeModel(config?.model || "");
      setKimiCodeType(config?.type ?? KIMI_CODE_DEFAULT_TYPE);
    },
    [],
  );

  return {
    kimiCodeProviderKey,
    setKimiCodeProviderKey,
    kimiCodeBaseUrl,
    kimiCodeApiKey,
    kimiCodeModel,
    kimiCodeType,
    existingKimiCodeKeys,
    handleKimiCodeBaseUrlChange,
    handleKimiCodeApiKeyChange,
    handleKimiCodeModelChange,
    handleKimiCodeTypeChange,
    resetKimiCodeState,
  };
}
