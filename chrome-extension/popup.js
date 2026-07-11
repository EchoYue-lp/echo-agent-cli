const connection = document.querySelector("#connection");
const title = document.querySelector("#title");
const url = document.querySelector("#url");
const message = document.querySelector("#message");
const authorize = document.querySelector("#authorize");
const revoke = document.querySelector("#revoke");
const developer = document.querySelector("#developer");

let currentTab = null;

function originPattern(value) {
  const parsed = new URL(value);
  if (parsed.protocol === "file:") return "file:///*";
  return `${parsed.protocol}//${parsed.host}/*`;
}

async function refresh() {
  const status = await chrome.runtime.sendMessage({ type: "status" });
  currentTab = status.tab ?? null;
  connection.textContent = status.connected ? "Connected" : "Desktop offline";
  connection.dataset.connected = String(Boolean(status.connected));
  title.textContent = currentTab?.title || "No active web tab";
  url.textContent = currentTab?.url || "";
  authorize.hidden = Boolean(status.authorized);
  revoke.hidden = !status.authorized;
  developer.hidden = Boolean(status.debuggerEnabled);
  message.textContent = status.authorized
    ? "This tab is authorized for EKO tasks."
    : "";
}

authorize.addEventListener("click", async () => {
  if (!currentTab?.id || !currentTab.url) return;
  try {
    const granted = await chrome.permissions.request({
      origins: [originPattern(currentTab.url)],
    });
    if (!granted) {
      message.textContent = "Website permission was not granted.";
      return;
    }
    const response = await chrome.runtime.sendMessage({
      type: "authorize_tab",
      tab: currentTab,
    });
    if (!response?.ok)
      throw new Error(response?.error || "Authorization failed.");
    await refresh();
  } catch (error) {
    message.textContent =
      error instanceof Error ? error.message : String(error);
  }
});

revoke.addEventListener("click", async () => {
  if (!currentTab?.id) return;
  const response = await chrome.runtime.sendMessage({
    type: "revoke_tab",
    tabId: currentTab.id,
  });
  if (!response?.ok) message.textContent = response?.error || "Release failed.";
  await refresh();
});

developer.addEventListener("click", async () => {
  const granted = await chrome.permissions.request({
    permissions: ["debugger"],
  });
  message.textContent = granted
    ? "Developer mode enabled."
    : "Developer mode was not enabled.";
  await refresh();
});

void refresh();
