import { useState, useCallback, useMemo } from "react";
import type { AppId } from "@/lib/api";
import { useProvidersQuery } from "@/lib/query/queries";
import {
  DSH_DEFAULT_API_MODE,
  type DshApiMode,
  type DshModel,
  type DshProviderSettingsConfig,
} from "@/config/dshProviderPresets";

interface UseDshFormStateParams {
  initialData?: {
    settingsConfig?: Record<string, unknown>;
  };
  appId: AppId;
  providerId?: string;
  onSettingsConfigChange: (config: string) => void;
  getSettingsConfig: () => string;
}

const DSH_DEFAULT_CONFIG_OBJ = {
  api: DSH_DEFAULT_API_MODE,
  baseURL: "",
  apiKey: "",
} as const;

export const DSH_DEFAULT_CONFIG = JSON.stringify(
  DSH_DEFAULT_CONFIG_OBJ,
  null,
  2,
);

export interface DshFormState {
  dshProviderKey: string;
  setDshProviderKey: (key: string) => void;
  dshBaseUrl: string;
  dshApiKey: string;
  dshApi: DshApiMode;
  dshModels: DshModel[];
  existingDshKeys: string[];
  handleDshBaseUrlChange: (baseUrl: string) => void;
  handleDshApiKeyChange: (apiKey: string) => void;
  handleDshApiChange: (mode: DshApiMode) => void;
  handleDshModelsChange: (models: DshModel[]) => void;
  resetDshState: (config?: Partial<DshProviderSettingsConfig>) => void;
}

function parseDshField<T>(
  initialData: UseDshFormStateParams["initialData"],
  field: string,
  fallback: T,
): T {
  try {
    if (initialData?.settingsConfig) {
      return (initialData.settingsConfig[field] as T) || fallback;
    }
    return (
      ((DSH_DEFAULT_CONFIG_OBJ as Record<string, unknown>)[field] as T) ||
      fallback
    );
  } catch {
    return fallback;
  }
}

export function useDshFormState({
  initialData,
  appId,
  providerId,
  onSettingsConfigChange,
  getSettingsConfig,
}: UseDshFormStateParams): DshFormState {
  const { data: dshProvidersData } = useProvidersQuery("dsh");
  const existingDshKeys = useMemo(() => {
    if (!dshProvidersData?.providers) return [];
    return Object.keys(dshProvidersData.providers).filter(
      (k) => k !== providerId,
    );
  }, [dshProvidersData?.providers, providerId]);

  const [dshProviderKey, setDshProviderKey] = useState<string>(() => {
    if (appId !== "dsh") return "";
    return providerId || "";
  });

  const [dshBaseUrl, setDshBaseUrl] = useState<string>(() => {
    if (appId !== "dsh") return "";
    return parseDshField(initialData, "baseURL", "");
  });

  const [dshApiKey, setDshApiKey] = useState<string>(() => {
    if (appId !== "dsh") return "";
    return parseDshField(initialData, "apiKey", "");
  });

  const [dshApi, setDshApi] = useState<DshApiMode>(() => {
    if (appId !== "dsh") return DSH_DEFAULT_API_MODE;
    const stored = parseDshField<DshApiMode | "">(initialData, "api", "");
    return stored || DSH_DEFAULT_API_MODE;
  });

  const [dshModels, setDshModels] = useState<DshModel[]>(() => {
    if (appId !== "dsh") return [];
    return parseDshField<DshModel[]>(initialData, "models", []);
  });

  const updateDshConfig = useCallback(
    (updater: (config: Record<string, unknown>) => void) => {
      try {
        const config = JSON.parse(getSettingsConfig() || DSH_DEFAULT_CONFIG);
        updater(config);
        onSettingsConfigChange(JSON.stringify(config, null, 2));
      } catch {
        // ignore
      }
    },
    [getSettingsConfig, onSettingsConfigChange],
  );

  const handleDshBaseUrlChange = useCallback(
    (baseUrl: string) => {
      setDshBaseUrl(baseUrl);
      updateDshConfig((config) => {
        config.baseURL = baseUrl.trim().replace(/\/+$/, "");
      });
    },
    [updateDshConfig],
  );

  const handleDshApiKeyChange = useCallback(
    (apiKey: string) => {
      setDshApiKey(apiKey);
      updateDshConfig((config) => {
        config.apiKey = apiKey;
      });
    },
    [updateDshConfig],
  );

  const handleDshApiChange = useCallback(
    (mode: DshApiMode) => {
      setDshApi(mode);
      updateDshConfig((config) => {
        config.api = mode;
      });
    },
    [updateDshConfig],
  );

  const handleDshModelsChange = useCallback(
    (models: DshModel[]) => {
      setDshModels(models);
      updateDshConfig((config) => {
        if (models.length === 0) {
          delete config.models;
        } else {
          config.models = models;
        }
      });
    },
    [updateDshConfig],
  );

  const resetDshState = useCallback(
    (config?: Partial<DshProviderSettingsConfig>) => {
      setDshProviderKey("");
      setDshBaseUrl(config?.baseURL || "");
      setDshApiKey(config?.apiKey || "");
      setDshApi(config?.api ?? DSH_DEFAULT_API_MODE);
      setDshModels(config?.models ?? []);
    },
    [],
  );

  return {
    dshProviderKey,
    setDshProviderKey,
    dshBaseUrl,
    dshApiKey,
    dshApi,
    dshModels,
    existingDshKeys,
    handleDshBaseUrlChange,
    handleDshApiKeyChange,
    handleDshApiChange,
    handleDshModelsChange,
    resetDshState,
  };
}
