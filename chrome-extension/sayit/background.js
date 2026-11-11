// Alt+L (or whatever is set at chrome://extensions/shortcuts) toggles the
// message in the active tab. Nothing is shown on screen.
chrome.commands.onCommand.addListener(async (command) => {
  if (command !== "toggle-message") return;
  // The options page's test mode listens for this directly.
  chrome.runtime.sendMessage({ sayit: "toggle" }).catch(() => {});
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (tab && tab.id !== undefined) chrome.tabs.sendMessage(tab.id, { sayit: "toggle" }).catch(() => {});
});

chrome.action.onClicked.addListener(() => chrome.runtime.openOptionsPage());
