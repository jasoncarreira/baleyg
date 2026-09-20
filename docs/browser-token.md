# Browser token convenience

Use **Load token file instead** if pasting into the password field does not work. Select
`.baleyg/native-smoke/daemon.token` for this inspector. The browser reads the selected file
locally; it fills only the password field and does not connect automatically.

Select **Remember token on this browser**, then **Connect**. Only a successful connection
saves the credential in this browser's localStorage for this exact origin (scheme/host/port).
On reload it pre-fills the token; select Connect without copying it again. It does not run
indexing or make provider calls. Remembering is optional and off by default.

This is browser credential storage, not an OS keychain. Use it only in a trusted browser
profile. Other scripts on the same origin can read it. Disconnect removes the saved token;
if storage access is blocked the UI reports that site data must be cleared manually.
Unchecking Remember also removes the saved value. Authentication rejection forgets it;
ordinary network failures do not. localhost and127.0.0.1 store separately.

Validation: real browser loaded the token file, saved after connection, restored a64-character
value after reload, reconnected without re-entry, and removed storage on Disconnect. Tests
use synthetic values; no credential value is logged or included in documentation.
