# EKO Browser Bridge

This Manifest V3 extension connects explicitly authorized Chrome tabs to the
local EKO desktop app through Chrome Native Messaging.

Desktop setup:

1. Choose `Connect Chrome...` in the EKO browser backend selector.
2. Open Chrome extensions and load the directory shown by EKO as an unpacked extension.
3. Copy the generated extension id into EKO and register the native host.
4. Restart the extension and confirm its popup shows `Connected`.
5. Open the popup on a website and authorize that site and tab.

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
