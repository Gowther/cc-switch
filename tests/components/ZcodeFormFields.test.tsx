import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { PropsWithChildren } from "react";
import { useRef, useState } from "react";
import { useForm } from "react-hook-form";
import { beforeAll, describe, expect, it, vi } from "vitest";
import { ZcodeFormFields } from "@/components/providers/forms/ZcodeFormFields";
import {
  useZcodeFormState,
  ZCODE_DEFAULT_CONFIG,
} from "@/components/providers/forms/hooks/useZcodeFormState";
import { Form } from "@/components/ui/form";

// useZcodeFormState 只用到 useProvidersQuery 来计算已占用 key，本测试不关心，
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

// ZcodeFormFields 是纯受控组件，settingsConfig 同步逻辑在 useZcodeFormState 里；
// 用真实 hook 搭一个最小 harness，验证「输入 → settingsConfig」的完整链路。
const ZcodeHarness = () => {
  const [settingsConfig, setSettingsConfig] = useState(ZCODE_DEFAULT_CONFIG);
  const settingsConfigRef = useRef(settingsConfig);
  settingsConfigRef.current = settingsConfig;

  const zcodeForm = useZcodeFormState({
    appId: "zcode",
    onSettingsConfigChange: (config: string) => setSettingsConfig(config),
    getSettingsConfig: () => settingsConfigRef.current,
  });

  return (
    <FormShell>
      <ZcodeFormFields
        baseUrl={zcodeForm.zcodeBaseUrl}
        onBaseUrlChange={zcodeForm.handleZcodeBaseUrlChange}
        apiKey={zcodeForm.zcodeApiKey}
        onApiKeyChange={zcodeForm.handleZcodeApiKeyChange}
        shouldShowApiKeyLink={false}
        websiteUrl=""
        kind={zcodeForm.zcodeKind}
        onKindChange={zcodeForm.handleZcodeKindChange}
        models={zcodeForm.zcodeModels}
        onModelsChange={zcodeForm.handleZcodeModelsChange}
      />
      <output data-testid="settings-config">{settingsConfig}</output>
    </FormShell>
  );
};

const readSettingsConfig = (): Record<string, unknown> => {
  const raw = screen.getByTestId("settings-config").textContent;
  return JSON.parse(raw ?? "{}");
};

describe("ZcodeFormFields", () => {
  beforeAll(() => {
    // Radix Select 打开下拉时会调用 scrollIntoView，jsdom 未实现
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("渲染 kind 下拉、API 端点与 API Key 输入，默认 kind 为 anthropic", () => {
    render(<ZcodeHarness />);

    // kind 下拉（Radix Select trigger 的 role 为 combobox）
    const kindTrigger = screen.getByRole("combobox");
    expect(kindTrigger).toHaveAttribute("id", "zcode-kind");

    // baseURL 输入
    const baseUrlInput = document.getElementById("zcode-baseurl");
    expect(baseUrlInput).not.toBeNull();
    expect(baseUrlInput).toHaveAttribute(
      "placeholder",
      "https://api.z.ai/api/anthropic",
    );

    // apiKey 输入（password 类型无 textbox role，按 id 定位）
    expect(document.getElementById("apiKey")).not.toBeNull();

    // 初始 settingsConfig 即携带默认 kind 键
    expect(readSettingsConfig().kind).toBe("anthropic");
  });

  it("修改 baseURL 后以 baseURL 键同步 settingsConfig（trim 并去除尾部斜杠）", () => {
    render(<ZcodeHarness />);

    const baseUrlInput = document.getElementById(
      "zcode-baseurl",
    ) as HTMLInputElement;
    fireEvent.change(baseUrlInput, {
      target: { value: "  https://api.z.ai/api/anthropic/  " },
    });

    const config = readSettingsConfig();
    expect(config.baseURL).toBe("https://api.z.ai/api/anthropic");
    // 其余键不受影响
    expect(config.kind).toBe("anthropic");
  });

  it("修改 apiKey 后同步 settingsConfig 的 apiKey 键", () => {
    render(<ZcodeHarness />);

    const apiKeyInput = document.getElementById("apiKey") as HTMLInputElement;
    fireEvent.change(apiKeyInput, { target: { value: "sk-zcode-test" } });

    expect(readSettingsConfig().apiKey).toBe("sk-zcode-test");
  });

  it("切换 kind 后写入 settingsConfig 的 kind 键", async () => {
    const user = userEvent.setup();
    render(<ZcodeHarness />);

    await user.click(screen.getByRole("combobox"));

    // i18n 测试资源为空，选项文案渲染为 labelKey
    const options = await screen.findAllByRole("option");
    expect(options).toHaveLength(3);

    await user.click(
      screen.getByRole("option", { name: "zcode.form.kindOpenaiCompatible" }),
    );

    await waitFor(() => {
      expect(readSettingsConfig().kind).toBe("openai-compatible");
    });
  });
});
