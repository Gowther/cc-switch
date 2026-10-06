import { useState, useCallback, useMemo } from "react";
import type { AppId } from "@/lib/api";
import { useProvidersQuery } from "@/lib/query/queries";
import {
  ZCODE_DEFAULT_KIND,
  type ZcodeKind,
  type ZcodeModel,
  type ZcodeProviderSettingsConfig,
} from "@/config/zcodeProviderPresets";

interface UseZcodeFormStateParams {
  initialData?: {
    settingsConfig?: Record<string, unknown>;
  };
  appId: AppId;
  providerId?: string;
  onSettingsConfigChange: (config: string) => void;
  getSettingsConfig: () => string;
}

const ZCODE_DEFAULT_CONFIG_OBJ = {
  kind: ZCODE_DEFAULT_KIND,
  baseURL: "",
  apiKey: "",
} as const;

export const ZCODE_DEFAULT_CONFIG = JSON.stringify(
  ZCODE_DEFAULT_CONFIG_OBJ,
  null,
  2,
);

export interface ZcodeFormState {
  zcodeProviderKey: string;
  setZcodeProviderKey: (key: string) => void;
  zcodeBaseUrl: string;
  zcodeApiKey: string;
  zcodeKind: ZcodeKind;
  zcodeModels: ZcodeModel[];
  existingZcodeKeys: string[];
  handleZcodeBaseUrlChange: (baseUrl: string) => void;
  handleZcodeApiKeyChange: (apiKey: string) => void;
  handleZcodeKindChange: (kind: ZcodeKind) => void;
  handleZcodeModelsChange: (models: ZcodeModel[]) => void;
  resetZcodeState: (config?: Partial<ZcodeProviderSettingsConfig>) => void;
}

function parseZcodeField<T>(
  initialData: UseZcodeFormStateParams["initialData"],
  field: string,
  fallback: T,
): T {
  try {
    if (initialData?.settingsConfig) {
      return (initialData.settingsConfig[field] as T) || fallback;
    }
    return (
      ((ZCODE_DEFAULT_CONFIG_OBJ as Record<string, unknown>)[field] as T) ||
      fallback
    );
  } catch {
    return fallback;
  }
}

export function useZcodeFormState({
  initialData,
  appId,
  providerId,
  onSettingsConfigChange,
  getSettingsConfig,
}: UseZcodeFormStateParams): ZcodeFormState {
  const { data: zcodeProvidersData } = useProvidersQuery("zcode");
  const existingZcodeKeys = useMemo(() => {
    if (!zcodeProvidersData?.providers) return [];
    return Object.keys(zcodeProvidersData.providers).filter(
      (k) => k !== providerId,
    );
  }, [zcodeProvidersData?.providers, providerId]);

  const [zcodeProviderKey, setZcodeProviderKey] = useState<string>(() => {
    if (appId !== "zcode") return "";
    return providerId || "";
  });

  const [zcodeBaseUrl, setZcodeBaseUrl] = useState<string>(() => {
    if (appId !== "zcode") return "";
    return parseZcodeField(initialData, "baseURL", "");
  });

  const [zcodeApiKey, setZcodeApiKey] = useState<string>(() => {
    if (appId !== "zcode") return "";
    return parseZcodeField(initialData, "apiKey", "");
  });

  const [zcodeKind, setZcodeKind] = useState<ZcodeKind>(() => {
    if (appId !== "zcode") return ZCODE_DEFAULT_KIND;
    const stored = parseZcodeField<ZcodeKind | "">(initialData, "kind", "");
    return stored || ZCODE_DEFAULT_KIND;
  });

  const [zcodeModels, setZcodeModels] = useState<ZcodeModel[]>(() => {
    if (appId !== "zcode") return [];
    return parseZcodeField<ZcodeModel[]>(initialData, "models", []);
  });

  const updateZcodeConfig = useCallback(
    (updater: (config: Record<string, unknown>) => void) => {
      try {
        const config = JSON.parse(getSettingsConfig() || ZCODE_DEFAULT_CONFIG);
        updater(config);
        onSettingsConfigChange(JSON.stringify(config, null, 2));
      } catch {
        // ignore
      }
    },
    [getSettingsConfig, onSettingsConfigChange],
  );

  const handleZcodeBaseUrlChange = useCallback(
    (baseUrl: string) => {
      setZcodeBaseUrl(baseUrl);
      updateZcodeConfig((config) => {
        config.baseURL = baseUrl.trim().replace(/\/+$/, "");
      });
    },
    [updateZcodeConfig],
  );

  const handleZcodeApiKeyChange = useCallback(
    (apiKey: string) => {
      setZcodeApiKey(apiKey);
      updateZcodeConfig((config) => {
        config.apiKey = apiKey;
      });
    },
    [updateZcodeConfig],
  );

  const handleZcodeKindChange = useCallback(
    (kind: ZcodeKind) => {
      setZcodeKind(kind);
      updateZcodeConfig((config) => {
        config.kind = kind;
      });
    },
    [updateZcodeConfig],
  );

  const handleZcodeModelsChange = useCallback(
    (models: ZcodeModel[]) => {
      setZcodeModels(models);
      updateZcodeConfig((config) => {
        if (models.length === 0) {
          delete config.models;
        } else {
          config.models = models;
        }
      });
    },
    [updateZcodeConfig],
  );

  const resetZcodeState = useCallback(
    (config?: Partial<ZcodeProviderSettingsConfig>) => {
      setZcodeProviderKey("");
      setZcodeBaseUrl(config?.baseURL || "");
      setZcodeApiKey(config?.apiKey || "");
      setZcodeKind(config?.kind ?? ZCODE_DEFAULT_KIND);
      setZcodeModels(config?.models ?? []);
    },
    [],
  );

  return {
    zcodeProviderKey,
    setZcodeProviderKey,
    zcodeBaseUrl,
    zcodeApiKey,
    zcodeKind,
    zcodeModels,
    existingZcodeKeys,
    handleZcodeBaseUrlChange,
    handleZcodeApiKeyChange,
    handleZcodeKindChange,
    handleZcodeModelsChange,
    resetZcodeState,
  };
}
