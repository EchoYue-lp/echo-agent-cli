# EKO Browser Bridge

This Manifest V3 extension connects explicitly authorized Chrome tabs to the
local EKO desktop app through Chrome Native Messaging.

Development setup:

1. Load this directory as an unpacked extension from `chrome://extensions`.
2. Copy the generated extension id and call the desktop
   `chrome_install_native_host` command (or use the future setup UI).
3. Restart the extension and confirm its popup shows `Connected`.
4. Open the popup on a website and authorize that site and tab.

The installed native-host manifest points at the EKO desktop executable itself.
When Chrome launches that executable with a `chrome-extension://` origin, EKO
enters bridge mode instead of opening a second desktop window. The standalone
`eko-chrome-native-host` binary exists only for development and protocol tests.

The default install does not request cookies, browsing history, or bookmarks.
Website access uses optional host permissions granted by the user from the
popup. Chrome debugger access is an optional permission with a separate popup
action; the bridge restricts it to a small read-oriented command allowlist.
Releasing an EKO task ungroups controlled tabs and does not close tabs that
existed before the task.
