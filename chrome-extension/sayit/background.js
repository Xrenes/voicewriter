importScripts("common.js");

// Every tab SayIt runs in: the built-in dialers plus the sites added in Settings.
async function dialerTabs() {
  const { origins = [] } = await chrome.permissions.getAll();
  return origins.length ? chrome.tabs.query({ url: origins }) : [];
}

// Toggle the message wherever a call is. Used by the optional "any tab"
// Chrome shortcut, the popup's button, and a hotkey pressed in a dialer tab
// that has no call. A hotkey pressed in the call's own tab never comes
// through here, so it stays instant.
async function toggleInDialers(exceptTab) {
  chrome.runtime.sendMessage({ sayit: "toggle" }).catch(() => {}); // the test page
  for (const tab of await dialerTabs()) {
    if (tab.id !== exceptTab) chrome.tabs.sendMessage(tab.id, { sayit: "toggle" }).catch(() => {});
  }
}

chrome.commands.onCommand.addListener((command) => {
  if (command === "toggle-message") toggleInDialers();
});

chrome.runtime.onMessage.addListener((m, sender) => {
  if (!m) return;
  if (m.sayit === "toggle-calls") toggleInDialers();
  else if (m.sayit === "toggle-other-tabs") toggleInDialers(sender.tab && sender.tab.id);
});

// Sites added in Settings (a white-label GHL, a CRM that embeds the dialer)
// get the same scripts as the built-in dialers once Chrome grants access.
const SITE_SCRIPTS = ["sayit-page", "sayit-relay"];
let syncing = Promise.resolve();

function syncAddedSites() {
  syncing = syncing
    .then(async () => {
      const { origins = [] } = await chrome.permissions.getAll();
      const sites = origins.filter((o) => !SAYIT_DIALERS.includes(o));
      const old = await chrome.scripting.getRegisteredContentScripts({ ids: SITE_SCRIPTS });
      if (old.length) await chrome.scripting.unregisterContentScripts({ ids: old.map((s) => s.id) });
      if (!sites.length) return;
      await chrome.scripting.registerContentScripts([
        { id: "sayit-page", matches: sites, js: ["inject.js"], runAt: "document_start", world: "MAIN", allFrames: true },
        { id: "sayit-relay", matches: sites, js: ["common.js", "relay.js"], runAt: "document_start", allFrames: true },
      ]);
    })
    .catch((e) => console.warn("SayIt: couldn't update the added sites", e));
  return syncing;
}

chrome.runtime.onInstalled.addListener(syncAddedSites);
chrome.runtime.onStartup.addListener(syncAddedSites);
chrome.permissions.onAdded.addListener(syncAddedSites);
chrome.permissions.onRemoved.addListener(syncAddedSites);
