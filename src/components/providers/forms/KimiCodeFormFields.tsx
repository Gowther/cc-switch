import { useTranslation } from "react-i18next";
import { useState, useCallback, useMemo } from "react";
import { FormLabel } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { toast } from "sonner";
import { Download, ChevronDown, Loader2 } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ApiKeySection } from "./shared";
import {
  fetchModelsForConfig,
  showFetchModelsError,
  type FetchedModel,
} from "@/lib/api/model-fetch";
import {
  kimiCodeProviderTypes,
  type KimiCodeProviderType,
} from "@/config/kimiCodeProviderPresets";
import type { ProviderCategory } from "@/types";

interface KimiCodeFormFieldsProps {
  baseUrl: string;
  onBaseUrlChange: (value: string) => void;
  apiKey: string;
  onApiKeyChange: (value: string) => void;
  category?: ProviderCategory;
  shouldShowApiKeyLink: boolean;
  websiteUrl: string;
  isPartner?: boolean;
  partnerPromotionKey?: string;
  providerType: KimiCodeProviderType;
  onProviderTypeChange: (type: KimiCodeProviderType) => void;
  model: string;
  onModelChange: (model: string) => void;
}

type BaseUrlErrorCode = "empty" | "invalid" | "scheme";

const BASE_URL_ERROR_I18N_KEY: Record<BaseUrlErrorCode, string> = {
  empty: "kimiCode.form.baseUrlRequired",
  scheme: "kimiCode.form.baseUrlScheme",
  invalid: "kimiCode.form.baseUrlInvalid",
};

const TEMPLATE_TOKEN_RE = /\$\{[^}]+\}/g;

/** Validate client-side so malformed endpoints surface before saving. */
function validateBaseUrl(raw: string): BaseUrlErrorCode | null {
  const trimmed = raw.trim();
  if (!trimmed) return "empty";
  // Presets may embed `${VAR}` tokens — swap them before URL parse.
  const candidate = trimmed.replace(TEMPLATE_TOKEN_RE, "placeholder");
  let u: URL;
  try {
    u = new URL(candidate);
  } catch {
    return "invalid";
  }
  if (!u.protocol.startsWith("http")) return "scheme";
  if (!u.hostname) return "invalid";
  return null;
}

export function KimiCodeFormFields({
  baseUrl,
  onBaseUrlChange,
  apiKey,
  onApiKeyChange,
  category,
  shouldShowApiKeyLink,
  websiteUrl,
  isPartner,
  partnerPromotionKey,
  providerType,
  onProviderTypeChange,
  model,
  onModelChange,
}: KimiCodeFormFieldsProps) {
  const { t } = useTranslation();
  const [fetchedModels, setFetchedModels] = useState<FetchedModel[]>([]);
  const [isFetchingModels, setIsFetchingModels] = useState(false);
  const [baseUrlTouched, setBaseUrlTouched] = useState(false);

  const baseUrlErrorCode = useMemo(() => validateBaseUrl(baseUrl), [baseUrl]);
  const showBaseUrlError = baseUrlTouched && baseUrlErrorCode !== null;
  const baseUrlErrorMessage = baseUrlErrorCode
    ? t(BASE_URL_ERROR_I18N_KEY[baseUrlErrorCode])
    : "";

  const groupedFetchedModels = useMemo(
    () =>
      Object.entries(
        fetchedModels.reduce(
          (acc, m) => {
            const v = m.ownedBy || "Other";
            if (!acc[v]) acc[v] = [];
            acc[v].push(m);
            return acc;
          },
          {} as Record<string, FetchedModel[]>,
        ),
      ).sort(([a], [b]) => a.localeCompare(b)),
    [fetchedModels],
  );

  const handleFetchModels = useCallback(() => {
    if (!baseUrl || !apiKey) {
      showFetchModelsError(null, t, {
        hasApiKey: !!apiKey,
        hasBaseUrl: !!baseUrl,
      });
      return;
    }
    setIsFetchingModels(true);
    fetchModelsForConfig(baseUrl, apiKey)
      .then((fetched) => {
        setFetchedModels(fetched);
        if (fetched.length === 0) {
          toast.info(t("providerForm.fetchModelsEmpty"));
        } else {
          toast.success(
            t("providerForm.fetchModelsSuccess", { count: fetched.length }),
          );
        }
      })
      .catch((err) => {
        console.warn("[ModelFetch] Failed:", err);
        showFetchModelsError(err, t);
      })
      .finally(() => setIsFetchingModels(false));
  }, [baseUrl, apiKey, t]);

  return (
    <>
      <div className="space-y-2">
        <FormLabel htmlFor="kimi-code-type">
          {t("kimiCode.form.type", { defaultValue: "API 类型" })}
        </FormLabel>
        <Select
          value={providerType}
          onValueChange={(v) => onProviderTypeChange(v as KimiCodeProviderType)}
        >
          <SelectTrigger id="kimi-code-type">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {kimiCodeProviderTypes.map((k) => (
              <SelectItem key={k.value} value={k.value}>
                {t(k.labelKey)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <p className="text-xs text-muted-foreground">
          {t("kimiCode.form.typeHint", {
            defaultValue: "供应商 API 协议（config.toml 的 type）。",
          })}
        </p>
      </div>

      <div className="space-y-2">
        <FormLabel htmlFor="kimi-code-baseurl">
          {t("kimiCode.form.baseUrl", { defaultValue: "API 端点" })}
        </FormLabel>
        <Input
          id="kimi-code-baseurl"
          value={baseUrl}
          onChange={(e) => onBaseUrlChange(e.target.value)}
          onBlur={() => setBaseUrlTouched(true)}
          placeholder="https://api.kimi.com/coding/v1"
          aria-invalid={showBaseUrlError}
          className={
            showBaseUrlError
              ? "border-destructive focus-visible:ring-destructive"
              : undefined
          }
        />
        {showBaseUrlError ? (
          <p className="text-xs text-destructive">{baseUrlErrorMessage}</p>
        ) : (
          <p className="text-xs text-muted-foreground">
            {t("kimiCode.form.baseUrlHint", {
              defaultValue: "供应商的 API 端点地址。",
            })}
          </p>
        )}
      </div>

      <ApiKeySection
        value={apiKey}
        onChange={onApiKeyChange}
        // Kimi Code 的 key 明文存 config.toml（官方行为），所有预设都需用户自填
        category={category === "official" ? undefined : category}
        shouldShowLink={shouldShowApiKeyLink}
        websiteUrl={websiteUrl}
        isPartner={isPartner}
        partnerPromotionKey={partnerPromotionKey}
      />

      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <FormLabel htmlFor="kimi-code-model">
            {t("kimiCode.form.model", { defaultValue: "模型" })}
          </FormLabel>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={handleFetchModels}
            disabled={isFetchingModels}
            className="h-7 gap-1"
          >
            {isFetchingModels ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Download className="h-3.5 w-3.5" />
            )}
            {t("providerForm.fetchModels")}
          </Button>
        </div>
        <div className="flex gap-1">
          <Input
            id="kimi-code-model"
            value={model}
            onChange={(e) => onModelChange(e.target.value)}
            placeholder={t("kimiCode.form.modelPlaceholder", {
              defaultValue: "kimi-for-coding",
            })}
            className="flex-1"
          />
          {fetchedModels.length > 0 && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="icon" className="shrink-0">
                  <ChevronDown className="h-4 w-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent
                align="end"
                className="max-h-64 overflow-y-auto z-[200]"
              >
                {groupedFetchedModels.map(([vendor, vModels], vi) => (
                  <div key={vendor}>
                    {vi > 0 && <DropdownMenuSeparator />}
                    <DropdownMenuLabel>{vendor}</DropdownMenuLabel>
                    {vModels.map((m) => (
                      <DropdownMenuItem
                        key={m.id}
                        onSelect={() => onModelChange(m.id)}
                      >
                        {m.id}
                      </DropdownMenuItem>
                    ))}
                  </div>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>
        <p className="text-xs text-muted-foreground">
          {t("kimiCode.form.modelHint", {
            defaultValue:
              "调用 API 时发送的模型 ID（config.toml 的 model）。上下文窗口、思考档位等可选属性可在下方 JSON 中编辑。",
          })}
        </p>
      </div>

      <p className="text-xs text-muted-foreground">
        {t("kimiCode.form.runtimeNote", {
          defaultValue:
            "配置写入 ~/.kimi-code/config.toml，对新建的 Kimi Code 会话生效。",
        })}
      </p>
    </>
  );
}
