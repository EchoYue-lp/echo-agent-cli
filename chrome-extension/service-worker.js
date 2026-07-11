const NATIVE_HOST = "com.eko.browser_bridge";
const MAX_SNAPSHOT_ITEMS = 200;

let nativePort = null;
let reconnectTimer = null;

function connectNative() {
  if (nativePort) return;
  try {
    const port = chrome.runtime.connectNative(NATIVE_HOST);
    nativePort = port;
    port.onMessage.addListener((message) => {
      void handleNativeRequest(message);
    });
    port.onDisconnect.addListener(() => {
      nativePort = null;
      scheduleReconnect();
    });
  } catch {
    nativePort = null;
    scheduleReconnect();
  }
}

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectNative();
  }, 2000);
}

async function sessionState() {
  const stored = await chrome.storage.session.get({
    authorizedTabs: {},
    tasks: {},
  });
  return {
    authorizedTabs: stored.authorizedTabs ?? {},
    tasks: stored.tasks ?? {},
  };
}

async function saveState(state) {
  await chrome.storage.session.set(state);
}

function originPattern(url) {
  const parsed = new URL(url);
  if (parsed.protocol === "file:") return "file:///*";
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new Error(
      "Only http, https, and explicitly enabled file pages are supported.",
    );
  }
  return `${parsed.protocol}//${parsed.host}/*`;
}

async function hasOriginPermission(url) {
  return chrome.permissions.contains({ origins: [originPattern(url)] });
}

async function authorizeTab(tab) {
  if (!tab?.id || !tab.url)
    throw new Error("The active Chrome tab is unavailable.");
  if (!(await hasOriginPermission(tab.url))) {
    throw new Error(
      "Website permission was not granted from the extension popup.",
    );
  }
  const state = await sessionState();
  state.authorizedTabs[String(tab.id)] = {
    id: tab.id,
    title: tab.title ?? "",
    url: tab.url,
    authorizedAt: new Date().toISOString(),
  };
  await saveState(state);
  return state.authorizedTabs[String(tab.id)];
}

async function revokeTab(tabId) {
  const state = await sessionState();
  delete state.authorizedTabs[String(tabId)];
  for (const task of Object.values(state.tasks)) {
    task.tabIds = task.tabIds.filter((id) => id !== tabId);
    task.createdTabIds = task.createdTabIds.filter((id) => id !== tabId);
    if (task.activeTabId === tabId) task.activeTabId = task.tabIds[0] ?? null;
  }
  await saveState(state);
}

async function authorizedTab(tabId) {
  const state = await sessionState();
  const authorized = state.authorizedTabs[String(tabId)];
  if (!authorized)
    throw new Error("Chrome tab is not authorized in the EKO extension.");
  const tab = await chrome.tabs.get(tabId);
  if (!tab.url || !(await hasOriginPermission(tab.url))) {
    throw new Error(
      "The current site is not authorized. Open the extension popup to allow it.",
    );
  }
  return tab;
}

function taskTitle(conversationId) {
  const compact = String(conversationId)
    .replace(/[^a-zA-Z0-9_-]/g, "")
    .slice(0, 18);
  return compact ? `EKO · ${compact}` : "EKO task";
}

async function claimTab(params) {
  const state = await sessionState();
  const requested = Number.isInteger(params.tabId) ? params.tabId : null;
  const tabId = requested ?? Number(Object.keys(state.authorizedTabs)[0]);
  if (!Number.isInteger(tabId)) {
    throw new Error(
      "Authorize a Chrome tab from the EKO extension popup first.",
    );
  }
  const tab = await authorizedTab(tabId);
  const groupId = await chrome.tabs.group({ tabIds: [tabId] });
  await chrome.tabGroups.update(groupId, {
    title: taskTitle(params.conversationId),
    color: "cyan",
    collapsed: false,
  });
  state.tasks[params.conversationId] = {
    groupId,
    tabIds: [tabId],
    createdTabIds: [],
    activeTabId: tabId,
  };
  await saveState(state);
  return { tabId, groupId, title: tab.title ?? "", url: tab.url ?? "" };
}

async function releaseTask(conversationId) {
  const state = await sessionState();
  const task = state.tasks[conversationId];
  if (!task) return { released: false };
  const existing = [];
  for (const tabId of task.tabIds) {
    try {
      await chrome.tabs.get(tabId);
      existing.push(tabId);
    } catch {
      // Closed by the user.
    }
  }
  if (existing.length) await chrome.tabs.ungroup(existing);
  delete state.tasks[conversationId];
  await saveState(state);
  return { released: true, tabIds: existing };
}

async function taskTab(conversationId) {
  const state = await sessionState();
  const task = state.tasks[conversationId];
  if (!task || !Number.isInteger(task.activeTabId)) {
    throw new Error(
      "This conversation has not selected an authorized Chrome tab.",
    );
  }
  const tab = await authorizedTab(task.activeTabId);
  return { state, task, tab };
}

async function executeInTab(tabId, func, args = []) {
  const results = await chrome.scripting.executeScript({
    target: { tabId },
    world: "ISOLATED",
    func,
    args,
  });
  return results[0]?.result;
}

function snapshotPage(limit) {
  function visible(element) {
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return (
      style.visibility !== "hidden" &&
      style.display !== "none" &&
      rect.width > 0 &&
      rect.height > 0
    );
  }
  function unique(selector) {
    try {
      return document.querySelectorAll(selector).length === 1;
    } catch {
      return false;
    }
  }
  function selectorFor(element) {
    if (element.id) {
      const selector = `#${CSS.escape(element.id)}`;
      if (unique(selector)) return selector;
    }
    for (const attribute of [
      "data-testid",
      "data-test",
      "aria-label",
      "name",
    ]) {
      const value = element.getAttribute(attribute);
      if (value) {
        const selector = `[${attribute}=${CSS.escape(value)}]`;
        if (unique(selector)) return selector;
      }
    }
    const parts = [];
    let current = element;
    while (
      current &&
      current.nodeType === Node.ELEMENT_NODE &&
      parts.length < 6
    ) {
      let part = current.tagName.toLowerCase();
      const parent = current.parentElement;
      if (parent) {
        const siblings = [...parent.children].filter(
          (child) => child.tagName === current.tagName,
        );
        if (siblings.length > 1)
          part += `:nth-of-type(${siblings.indexOf(current) + 1})`;
      }
      parts.unshift(part);
      const selector = parts.join(" > ");
      if (unique(selector)) return selector;
      current = parent;
    }
    return parts.join(" > ");
  }
  const candidates = document.querySelectorAll(
    'a[href],button,input,textarea,select,[role="button"],[role="link"],[contenteditable="true"]',
  );
  const items = [];
  for (const element of candidates) {
    if (items.length >= limit || !visible(element)) continue;
    const text = (
      element.innerText ||
      element.value ||
      element.getAttribute("aria-label") ||
      ""
    )
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 160);
    items.push({
      ref: selectorFor(element),
      role: element.getAttribute("role") || element.tagName.toLowerCase(),
      text,
      href: element.href || undefined,
      type: element.type || undefined,
    });
  }
  return { title: document.title, url: location.href, items };
}

function clickTarget(selector, doubleClick) {
  const element = document.querySelector(selector);
  if (!element) throw new Error("Snapshot target no longer exists.");
  element.scrollIntoView({ block: "center", inline: "center" });
  if (doubleClick) {
    element.click();
    element.click();
    element.dispatchEvent(
      new MouseEvent("dblclick", {
        bubbles: true,
        cancelable: true,
        view: window,
      }),
    );
  } else {
    element.click();
  }
  return { clicked: true };
}

function fillTarget(selector, text, submit) {
  const element = document.querySelector(selector);
  if (!element) throw new Error("Snapshot target no longer exists.");
  element.focus();
  if ("value" in element) {
    const prototype = Object.getPrototypeOf(element);
    const descriptor = prototype
      ? Object.getOwnPropertyDescriptor(prototype, "value")
      : null;
    if (descriptor?.set) descriptor.set.call(element, text);
    else element.value = text;
  } else element.textContent = text;
  element.dispatchEvent(
    new InputEvent("input", {
      bubbles: true,
      inputType: "insertText",
      data: text,
    }),
  );
  element.dispatchEvent(new Event("change", { bubbles: true }));
  if (submit) {
    const form = element.closest("form");
    if (!form) throw new Error("The target is not inside a form.");
    form.requestSubmit();
  }
  return { filled: true, submitted: Boolean(submit) };
}

async function screenshotTab(tab) {
  if (!Number.isInteger(tab.windowId))
    throw new Error("Chrome tab window is unavailable.");
  if (!tab.active) await chrome.tabs.update(tab.id, { active: true });
  const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, {
    format: "png",
  });
  const comma = dataUrl.indexOf(",");
  if (comma < 0) throw new Error("Chrome returned an invalid screenshot.");
  return {
    data: dataUrl.slice(comma + 1),
    mimeType: "image/png",
    url: tab.url ?? "",
    title: tab.title ?? "",
  };
}

async function runBrowserAction(method, conversationId, params) {
  if (method === "interrupt") return { interrupted: true };
  const { state, task, tab } = await taskTab(conversationId);
  if (method === "navigate") {
    if (!(await hasOriginPermission(params.url))) {
      throw new Error(
        "Destination site is not authorized. Allow it from the extension popup first.",
      );
    }
    const updated = await chrome.tabs.update(tab.id, { url: params.url });
    return { url: updated.url ?? params.url, title: updated.title ?? "" };
  }
  if (method === "snapshot") {
    const snapshot = await executeInTab(tab.id, snapshotPage, [
      MAX_SNAPSHOT_ITEMS,
    ]);
    return { ...snapshot, text: JSON.stringify(snapshot, null, 2) };
  }
  if (method === "click") {
    const result = await executeInTab(tab.id, clickTarget, [
      params.target,
      Boolean(params.doubleClick),
    ]);
    const updated = await chrome.tabs.get(tab.id);
    return { ...result, url: updated.url ?? "", title: updated.title ?? "" };
  }
  if (method === "fill") {
    const result = await executeInTab(tab.id, fillTarget, [
      params.target,
      params.text,
      Boolean(params.submit),
    ]);
    const updated = await chrome.tabs.get(tab.id);
    return { ...result, url: updated.url ?? "", title: updated.title ?? "" };
  }
  if (method === "screenshot") return screenshotTab(tab);
  if (method === "cdp_command") {
    const allowed = new Set([
      "Runtime.enable",
      "Log.enable",
      "Network.enable",
      "Performance.enable",
      "Performance.getMetrics",
    ]);
    if (!allowed.has(params.command))
      throw new Error("CDP command is not allowed by the EKO bridge.");
    if (!(await chrome.permissions.contains({ permissions: ["debugger"] }))) {
      throw new Error(
        "Enable developer mode from the EKO extension popup first.",
      );
    }
    const target = { tabId: tab.id };
    await chrome.debugger.attach(target, "1.3");
    try {
      return await chrome.debugger.sendCommand(
        target,
        params.command,
        params.parameters ?? {},
      );
    } finally {
      await chrome.debugger.detach(target);
    }
  }
  if (method === "back") {
    await chrome.tabs.goBack(tab.id);
    return { url: (await chrome.tabs.get(tab.id)).url ?? "" };
  }
  if (method === "reload") {
    await chrome.tabs.reload(tab.id);
    return { url: (await chrome.tabs.get(tab.id)).url ?? "" };
  }
  if (method === "scroll") {
    await executeInTab(
      tab.id,
      (deltaX, deltaY) => window.scrollBy(deltaX, deltaY),
      [params.deltaX ?? 0, params.deltaY ?? 0],
    );
    return { scrolled: true, url: tab.url ?? "" };
  }
  if (method === "tabs") {
    if (params.action === "list") {
      const tabs = [];
      for (const tabId of task.tabIds) {
        try {
          const item = await chrome.tabs.get(tabId);
          tabs.push({
            id: item.id,
            title: item.title ?? "",
            url: item.url ?? "",
            active: item.id === task.activeTabId,
          });
        } catch {
          // Closed by the user.
        }
      }
      return { tabs };
    }
    if (params.action === "new") {
      const url = params.url ?? "about:blank";
      if (url !== "about:blank" && !(await hasOriginPermission(url))) {
        throw new Error("New tab destination is not authorized.");
      }
      const created = await chrome.tabs.create({ url, active: false });
      await chrome.tabs.group({ groupId: task.groupId, tabIds: [created.id] });
      task.tabIds.push(created.id);
      task.createdTabIds.push(created.id);
      task.activeTabId = created.id;
      state.authorizedTabs[String(created.id)] = {
        id: created.id,
        title: created.title ?? "",
        url,
        authorizedAt: new Date().toISOString(),
      };
      await saveState(state);
      return { tabId: created.id, url };
    }
    const index = Number(params.index);
    const tabId = task.tabIds[index];
    if (!Number.isInteger(tabId))
      throw new Error("Chrome task tab index does not exist.");
    if (params.action === "select") {
      await authorizedTab(tabId);
      task.activeTabId = tabId;
      await chrome.tabs.update(tabId, { active: true });
      await saveState(state);
      return { selected: tabId };
    }
    if (params.action === "close") {
      if (!task.createdTabIds.includes(tabId)) {
        throw new Error(
          "EKO will not close a Chrome tab that existed before this task.",
        );
      }
      await chrome.tabs.remove(tabId);
      task.tabIds = task.tabIds.filter((id) => id !== tabId);
      task.createdTabIds = task.createdTabIds.filter((id) => id !== tabId);
      task.activeTabId = task.tabIds[0] ?? null;
      delete state.authorizedTabs[String(tabId)];
      await saveState(state);
      return { closed: tabId };
    }
  }
  throw new Error(`Unsupported Chrome browser action: ${method}`);
}

async function handleNativeRequest(message) {
  if (!message?.id || !message?.method) return;
  try {
    let result;
    if (message.method === "claim_tab")
      result = await claimTab(message.params ?? {});
    else if (message.method === "release_task")
      result = await releaseTask(message.params?.conversationId);
    else
      result = await runBrowserAction(
        message.method,
        message.params?.conversationId,
        message.params?.params ?? {},
      );
    nativePort?.postMessage({ id: message.id, result });
  } catch (error) {
    nativePort?.postMessage({
      id: message.id,
      error: error instanceof Error ? error.message : String(error),
    });
  }
}

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  void (async () => {
    if (message.type === "status") {
      const [tab] = await chrome.tabs.query({
        active: true,
        currentWindow: true,
      });
      const state = await sessionState();
      sendResponse({
        connected: Boolean(nativePort),
        tab,
        authorized: Boolean(tab?.id && state.authorizedTabs[String(tab.id)]),
        debuggerEnabled: await chrome.permissions.contains({
          permissions: ["debugger"],
        }),
      });
      return;
    }
    if (message.type === "authorize_tab")
      sendResponse({ ok: true, tab: await authorizeTab(message.tab) });
    else if (message.type === "revoke_tab") {
      await revokeTab(message.tabId);
      sendResponse({ ok: true });
    }
  })().catch((error) =>
    sendResponse({
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    }),
  );
  return true;
});

chrome.tabs.onRemoved.addListener((tabId) => {
  void revokeTab(tabId);
});

connectNative();
