import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { PropsWithChildren } from "react";
import { useRef, useState } from "react";
import { useForm } from "react-hook-form";
import { beforeAll, describe, expect, it, vi } from "vitest";
import { DshFormFields } from "@/components/providers/forms/DshFormFields";
import {
  DSH_DEFAULT_CONFIG,
  useDshFormState,
} from "@/components/providers/forms/hooks/useDshFormState";
import { Form } from "@/components/ui/form";

// useDshFormState 只用到 useProvidersQuery 来计算已占用 key，本测试不关心，
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

// DshFormFields 是纯受控组件，settingsConfig 同步逻辑在 useDshFormState 里；
// 用真实 hook 搭一个最小 harness，验证「输入 → settingsConfig」的完整链路。
const DshHarness = () => {
  const [settingsConfig, setSettingsConfig] = useState(DSH_DEFAULT_CONFIG);
  const settingsConfigRef = useRef(settingsConfig);
  settingsConfigRef.current = settingsConfig;

  const dshForm = useDshFormState({
    appId: "dsh",
    onSettingsConfigChange: (config: string) => setSettingsConfig(config),
    getSettingsConfig: () => settingsConfigRef.current,
  });

  return (
    <FormShell>
      <DshFormFields
        baseUrl={dshForm.dshBaseUrl}
        onBaseUrlChange={dshForm.handleDshBaseUrlChange}
        apiKey={dshForm.dshApiKey}
        onApiKeyChange={dshForm.handleDshApiKeyChange}
        shouldShowApiKeyLink={false}
        websiteUrl=""
        api={dshForm.dshApi}
        onApiChange={dshForm.handleDshApiChange}
        models={dshForm.dshModels}
        onModelsChange={dshForm.handleDshModelsChange}
      />
      <output data-testid="settings-config">{settingsConfig}</output>
    </FormShell>
  );
};

const readSettingsConfig = (): Record<string, unknown> => {
  const raw = screen.getByTestId("settings-config").textContent;
  return JSON.parse(raw ?? "{}");
};

describe("DshFormFields", () => {
  beforeAll(() => {
    // Radix Select 打开下拉时会调用 scrollIntoView，jsdom 未实现
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("渲染 api 下拉、API 端点与 API Key 输入，默认协议为 openai-completions", () => {
    render(<DshHarness />);

    // api 协议下拉（Radix Select trigger 的 role 为 combobox）
    const apiTrigger = screen.getByRole("combobox");
    expect(apiTrigger).toHaveAttribute("id", "dsh-api-mode");

    // baseURL 输入
    const baseUrlInput = document.getElementById("dsh-baseurl");
    expect(baseUrlInput).not.toBeNull();
    expect(baseUrlInput).toHaveAttribute(
      "placeholder",
      "https://api.deepseek.com",
    );

    // apiKey 输入（password 类型无 textbox role，按 id 定位）
    expect(document.getElementById("apiKey")).not.toBeNull();

    // 初始 settingsConfig 即携带默认 api 键
    expect(readSettingsConfig().api).toBe("openai-completions");
  });

  it("修改 baseURL 后以 baseURL 键同步 settingsConfig（trim 并去除尾部斜杠）", () => {
    render(<DshHarness />);

    const baseUrlInput = document.getElementById(
      "dsh-baseurl",
    ) as HTMLInputElement;
    fireEvent.change(baseUrlInput, {
      target: { value: "  https://api.deepseek.com/  " },
    });

    const config = readSettingsConfig();
    expect(config.baseURL).toBe("https://api.deepseek.com");
    // 其余键不受影响
    expect(config.api).toBe("openai-completions");
  });

  it("修改 apiKey 后同步 settingsConfig 的 apiKey 键", () => {
    render(<DshHarness />);

    const apiKeyInput = document.getElementById("apiKey") as HTMLInputElement;
    fireEvent.change(apiKeyInput, { target: { value: "sk-test-123" } });

    expect(readSettingsConfig().apiKey).toBe("sk-test-123");
  });

  it("切换 api 协议后写入 settingsConfig 的 api 键", async () => {
    const user = userEvent.setup();
    render(<DshHarness />);

    await user.click(screen.getByRole("combobox"));

    // i18n 测试资源为空，选项文案渲染为 labelKey
    const options = await screen.findAllByRole("option");
    expect(options).toHaveLength(3);

    await user.click(
      screen.getByRole("option", { name: "dsh.form.apiAnthropicMessages" }),
    );

    await waitFor(() => {
      expect(readSettingsConfig().api).toBe("anthropic-messages");
    });
  });
});
