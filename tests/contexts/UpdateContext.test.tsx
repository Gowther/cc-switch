import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { UpdateProvider, useUpdate } from "@/contexts/UpdateContext";

const checkForUpdateMock = vi.hoisted(() => vi.fn());

vi.mock("@/lib/updater", () => ({
  checkForUpdate: (...args: unknown[]) => checkForUpdateMock(...args),
}));

function UpdateConsumer() {
  const { checkUpdate, hasUpdate } = useUpdate();

  return (
    <>
      <button type="button" onClick={() => void checkUpdate()}>
        check
      </button>
      <span>{hasUpdate ? "available" : "idle"}</span>
    </>
  );
}

describe("UpdateProvider", () => {
  beforeEach(() => {
    checkForUpdateMock.mockReset();
    checkForUpdateMock.mockResolvedValue({ status: "up-to-date" });
    localStorage.clear();
  });

  it("checks for updates only after an explicit user action", async () => {
    render(
      <UpdateProvider>
        <UpdateConsumer />
      </UpdateProvider>,
    );

    expect(checkForUpdateMock).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "check" }));

    await waitFor(() => expect(checkForUpdateMock).toHaveBeenCalledTimes(1));
    expect(screen.getByText("idle")).toBeInTheDocument();
  });
});
