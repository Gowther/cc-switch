import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { PropsWithChildren } from "react";
import { useRef, useState } from "react";
import { useForm } from "react-hook-form";
import { beforeAll, describe, expect, it, vi } from "vitest";
import { KimiCodeFormFields } from "@/components/providers/forms/KimiCodeFormFields";
import {
  useKimiCodeFormState,
  KIMI_CODE_DEFAULT_CONFIG,
} from "@/components/providers/forms/hooks/useKimiCodeFormState";
import { Form } from "@/components/ui/form";

// useKimiCodeFormState 只用到 useProvidersQuery 来计算已占用 key，本测试不关心，
// mock 掉以免引入 QueryClientProvider 与后端 invoke。
vi.mock("@/lib/query/queries", () => ({
  useProvidersQuery: () => ({ data: undefined }),
}));

// 模型拉取走 Tauri 后端，测试不触发；mock 仅为隔离 import 副作用。
vi.mock("@/lib/api/model-fetch", () => ({
  fetchModelsForConfig: vi.fn().mockResolvedValue([]),
  showFetchModelsError: vi.fn(),
}));

const FormShell = ({ children }: PropsWithChildren) => {
  const form = useForm();

  return <Form {...form}>{children}</Form>;
};

// KimiCodeFormFields 是纯受控组件，settingsConfig 同步逻辑在 useKimiCodeFormState 里；
// 用真实 hook 搭一个最小 harness，验证「输入 → settingsConfig」的完整链路。
const KimiCodeHarness = () => {
  const [settingsConfig, setSettingsConfig] = useState(
    KIMI_CODE_DEFAULT_CONFIG,
  );
  const settingsConfigRef = useRef(settingsConfig);
  settingsConfigRef.current = settingsConfig;

  const kimiCodeForm = useKimiCodeFormState({
    appId: "kimi-code",
    onSettingsConfigChange: (config: string) => setSettingsConfig(config),
    getSettingsConfig: () => settingsConfigRef.current,
  });

  return (
    <FormShell>
      <KimiCodeFormFields
        baseUrl={kimiCodeForm.kimiCodeBaseUrl}
        onBaseUrlChange={kimiCodeForm.handleKimiCodeBaseUrlChange}
        apiKey={kimiCodeForm.kimiCodeApiKey}
        onApiKeyChange={kimiCodeForm.handleKimiCodeApiKeyChange}
        shouldShowApiKeyLink={false}
        websiteUrl=""
        providerType={kimiCodeForm.kimiCodeType}
        onProviderTypeChange={kimiCodeForm.handleKimiCodeTypeChange}
        model={kimiCodeForm.kimiCodeModel}
        onModelChange={kimiCodeForm.handleKimiCodeModelChange}
      />
      <output data-testid="settings-config">{settingsConfig}</output>
    </FormShell>
  );
};

const readSettingsConfig = (): Record<string, unknown> => {
  const raw = screen.getByTestId("settings-config").textContent;
  return JSON.parse(raw ?? "{}");
};

describe("KimiCodeFormFields", () => {
  beforeAll(() => {
    // Radix Select 打开下拉时会调用 scrollIntoView，jsdom 未实现
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("渲染 type 下拉、API 端点与 API Key 输入，默认 type 为 anthropic", () => {
    render(<KimiCodeHarness />);

    // type 下拉（Radix Select trigger 的 role 为 combobox）
    const typeTrigger = screen.getByRole("combobox");
    expect(typeTrigger).toHaveAttribute("id", "kimi-code-type");

    // baseURL 输入
    const baseUrlInput = document.getElementById("kimi-code-baseurl");
    expect(baseUrlInput).not.toBeNull();
    expect(baseUrlInput).toHaveAttribute(
      "placeholder",
      "https://api.kimi.com/coding/v1",
    );

    // apiKey 输入（password 类型无 textbox role，按 id 定位）
    expect(document.getElementById("apiKey")).not.toBeNull();

    // 初始 settingsConfig 即携带默认 type 键
    expect(readSettingsConfig().type).toBe("anthropic");
  });

  it("修改 baseURL 后以 base_url 键同步 settingsConfig（trim 并去除尾部斜杠）", () => {
    render(<KimiCodeHarness />);

    const baseUrlInput = document.getElementById(
      "kimi-code-baseurl",
    ) as HTMLInputElement;
    fireEvent.change(baseUrlInput, {
      target: { value: "  https://api.kimi.com/coding/v1/  " },
    });

    const config = readSettingsConfig();
    expect(config.base_url).toBe("https://api.kimi.com/coding/v1");
    // 其余键不受影响
    expect(config.type).toBe("anthropic");
  });

  it("修改 apiKey 后以 snake_case api_key 键同步 settingsConfig", () => {
    render(<KimiCodeHarness />);

    const apiKeyInput = document.getElementById("apiKey") as HTMLInputElement;
    fireEvent.change(apiKeyInput, { target: { value: "sk-zcode-test" } });

    expect(readSettingsConfig().api_key).toBe("sk-zcode-test");
  });

  it("切换 type 后写入 settingsConfig 的 type 键", async () => {
    const user = userEvent.setup();
    render(<KimiCodeHarness />);

    await user.click(screen.getByRole("combobox"));

    // i18n 测试资源为空，选项文案渲染为 labelKey
    const options = await screen.findAllByRole("option");
    expect(options.length).toBeGreaterThanOrEqual(3);

    await user.click(
      screen.getByRole("option", {
        name: "kimiCode.form.typeOpenaiResponses",
      }),
    );

    await waitFor(() => {
      expect(readSettingsConfig().type).toBe("openai_responses");
    });
  });

  it("修改模型后以 model 键同步 settingsConfig", () => {
    render(<KimiCodeHarness />);

    const modelInput = document.getElementById(
      "kimi-code-model",
    ) as HTMLInputElement;
    fireEvent.change(modelInput, { target: { value: "kimi-k2.7-code" } });

    expect(readSettingsConfig().model).toBe("kimi-k2.7-code");
  });
});
