import { act, renderHook, waitFor } from "@testing-library/react";
import { useMemo, useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { configApi } from "@/lib/api";
import { useCodexCommonConfig } from "./useCodexCommonConfig";

vi.mock("@/lib/api", () => ({
  configApi: {
    getCommonConfigSnippet: vi.fn(),
    updateTomlCommonConfigSnippet: vi.fn(),
    setCommonConfigSnippet: vi.fn(),
    extractCommonConfigSnippet: vi.fn(),
  },
}));

const providerDelta = 'model_provider = "custom"\n';
const commonSnippet = 'model = "shared"\n';

function useHarness() {
  const [config, setConfig] = useState(providerDelta);
  const initialData = useMemo(
    () => ({ settingsConfig: { config: providerDelta } }),
    [],
  );
  const common = useCodexCommonConfig({
    codexConfig: config,
    onConfigChange: setConfig,
    initialData,
    initialEnabled: true,
    selectedPresetId: "custom",
  });

  return { ...common, config, setConfig };
}

describe("useCodexCommonConfig", () => {
  beforeEach(() => {
    vi.mocked(configApi.getCommonConfigSnippet).mockResolvedValue(
      commonSnippet,
    );
    vi.mocked(configApi.setCommonConfigSnippet).mockResolvedValue();
  });

  it("keeps the persisted enabled state when live config arrives during preview hydration", async () => {
    let resolveMerge: ((value: string) => void) | undefined;
    const mergePromise = new Promise<string>((resolve) => {
      resolveMerge = resolve;
    });
    vi.mocked(configApi.updateTomlCommonConfigSnippet).mockReturnValue(
      mergePromise,
    );

    const { result } = renderHook(() => useHarness());

    await waitFor(() => {
      expect(configApi.updateTomlCommonConfigSnippet).toHaveBeenCalledTimes(1);
    });

    // The database flag is the source of truth, even while the preview merge
    // is still pending.
    expect(result.current.useCommonConfig).toBe(true);
    expect(result.current.isCommonConfigBusy).toBe(true);

    // EditProviderDialog replaces the provider delta with the effective live
    // config asynchronously. That must not flip the checkbox back to false.
    act(() => {
      result.current.setConfig(`${providerDelta}${commonSnippet}`);
    });

    if (!resolveMerge) {
      throw new Error("expected the preview merge to be pending");
    }
    const mergeResolver = resolveMerge;
    await act(async () => {
      mergeResolver(`${providerDelta}${commonSnippet}`);
      await mergePromise;
    });

    await waitFor(() => {
      expect(result.current.isCommonConfigBusy).toBe(false);
    });
    expect(result.current.useCommonConfig).toBe(true);
  });
});
