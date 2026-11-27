# Router API — Reverse-engineering notes

Live device: `http://192.168.0.1`

## Identification

| Field | Value |
|-------|-------|
| Model | MC60 (branded "olax") |
| Hardware | A1 |
| Firmware | MC60_V1.0.2 |
| HTTP server | Boa/0.94.14rc21 (root `/` reports `micro_httpd`) |
| Platform | Realtek Semiconductor (copyright headers in pages) |
| Type | 4G LTE router (has Mobile network / USSD / SMS) |

## Authentication (challenge–response HMAC-MD5)

Session is tracked by a single cookie: `webuicookie`. No CSRF token was
observed — the session cookie is the only credential after login.

Login is a 3-step flow:

1. **Request challenge**
   - `POST /boafrm/formLoginKey`
   - `Content-Type: application/x-www-form-urlencoded`
   - body: `username=admin`
   - response JSON: `{"Challenge":"<20 chars>","PublicKey":"<20 chars>"}`

2. **Compute password proof** (must match the JS in `js/hmac_md5.js`):
   - `PrivateKey = HMAC-MD5(key = PublicKey + password, msg = Challenge)` → uppercase hex
   - `Login_Passwd = HMAC-MD5(key = PrivateKey, msg = Challenge)` → uppercase hex
   - All inputs are UTF-8 encoded; HMAC is standard RFC 2104 MD5.

3. **Submit login**
   - `POST /boafrm/formLoginSetup`
   - body: `username=admin&password=<Login_Passwd>`
   - response sets `Set-Cookie: webuicookie=<token>; path=/` and redirects to `/main.html`.

### Logout
`POST /boafrm/logoutSetup`

## Session-state guard (important)

Form POSTs are only accepted after the session has loaded the **corresponding
page**. For example, `formSaveConfig` (reboot/reset) returns the login page
unless `/saveconf.htm` was fetched first, and `formUSSDSetup` needs `/ussd.htm`
loaded first. The CLI therefore GETs the relevant page before each form POST.

Form POSTs also reply with a `302` redirect to the page's `submit-url` field;
the CLI follows it to read the re-rendered result (e.g. the USSD response).

## Session / cookies
- Send `Cookie: webuicookie=<token>` on every request after login.
- The token is returned by `formLoginSetup` (a fresh one per login).

## Endpoints

| Feature | Page | Form action | Method |
|---------|------|-------------|--------|
| Reboot | saveconf.htm | `/boafrm/formSaveConfig` | POST |
| Factory reset | saveconf.htm | `/boafrm/formSaveConfig` | POST |
| USSD | ussd.htm | `/boafrm/formUSSDSetup` | POST |
| WAN | wanconfig.htm | `/boafrm/formWanConfigSetup` | POST |
| LAN | tcpiplan.htm | `/boafrm/formTcpipSetup` | POST |
| WiFi | wlbasic.htm | `/boafrm/formWlanSetup` | POST |
| SMS inbox | sms_inbox.htm | `/boafrm/formSmsManage` | GET |

### Reboot
`POST /boafrm/formSaveConfig` with body `reboot=Reboot`.
Device restarts (connection drops, then returns).

### Factory reset
`POST /boafrm/formSaveConfig` with body `reset=Reset`.
(Dangerous — restores defaults, wipes config.)

### USSD
`POST /boafrm/formUSSDSetup` (`application/x-www-form-urlencoded`), fields:

| Field | Meaning |
|-------|---------|
| `ussdValue` | USSD code to dial (e.g. `*825#`) or menu option (`1`) |
| `ussdStatusInput` | `ussd` (normal send) or `menu` (interactive session) |
| `ussdCancelInput` | `0` = send, `1` = cancel session |
| `selectMenuValue` | chosen option number during an interactive USSD menu |
| `submit-url` | redirect target, `/ussd.htm` |

The response re-renders `ussd.htm` with a `ussdStatus` flag:
`0` = idle/send form, `1` = interactive menu (result is a numbered list),
`2` = final result text. The result is embedded in the returned HTML:
`ussd_result_id` wraps `ussd_menu_id`, and menu options are separated by
`<br>` (so they must be kept as separate lines when scraping).

Notes:
- Replies to an open menu are sent with `ussdStatusInput=ussd` and the option
  (e.g. `1`) in `ussdValue`; the session stays open and the page keeps
  `ussdStatus = '1'`.
- Cancelling an open session is
  `ussdValue=&selectMenuValue=&ussdStatusInput=menu&ussdCancelInput=1`;
  afterwards `ussdStatus` returns to `0`.
- If a new dial is sent while an old session is still open, the device displays
  the literal text `Unexpected Data Value` in the menu div instead of a carrier
  response. Cancelling first avoids this.

### SMS inbox (read-only)
`GET /sms_inbox.htm` returns the whole inbox embedded in a JavaScript
string literal (no extra AJAX call is needed):

```js
var smsListInfo = "15,16,17}-{0}-{CAMTEL}-{2026-11-26 16:16:41}-{Your balance is ...}-{|,|14}-{0}-{...}-{...}-{...}-{|,|";
```

Record layout — records separated by `|,|`, fields by `}-{`:

| # | Field | Example | Notes |
|---|-------|---------|-------|
| 0 | `index` | `15,16,17` | modem storage indices; comma-separated when a long SMS arrived as multiple parts (concatenated into one row) |
| 1 | `stat` | `0` | `0` = unread (row rendered bold), `1` = read |
| 2 | `number` | `CAMTEL` | sender, network name or MSISDN |
| 3 | `time` | `2026-11-26 16:16:41` | device-local time, `YYYY-MM-DD HH:MM:SS` |
| 4 | `content` | `Cher client, ...` | body text, HTML-escaped (e.g. `&#39;`) |

The list is **not** sorted: the device emits storage order, which is roughly
newest-first but not monotonic (same-second messages appear in arbitrary order),
so sort by `time` for "last N". Content is truncated to the device's per-part
buffer (~160 chars per SMS) — the page shows no more than this.

Getting `sms_inbox.htm` is side-effect free: it does not mark messages read.
No session-state guard applies here (unlike `ussd.htm` / `saveconf.htm`): a fresh
login can fetch `sms_inbox.htm` directly, although the UI reaches it through
Mobile network → SMS (`lte.htm`).
Reading a single message (which does mark it read) is
`POST /boafrm/formSmsManage` with `action_id=readMsg`,
`action_value=<index>`, `submit-url=/sms_inbox.htm` — deliberately not used by the
CLI. It only flips `stat` to `1` and redirects back to the list; it does not
return a larger body.

There is no detail endpoint: `sms_read.htm`, `sms_view.htm`, `sms_detail.htm`,
`sms_msg.htm`, `/sms_inbox.htm?index=<i>`, and `readMsg`/`getMsg`/`viewMsg`
variants either 404/400 or only mark the message read. The `smsListInfo` string
above is therefore the only source of message text over HTTP — the CLI's
`--full` flag re-wraps that text, it cannot recover characters the firmware
truncated.

Deleting uses the same action with `action_id=delete` and a comma-separated
`action_value` of indices.

### Config GET (read current values)
The `.htm` pages are server-side templates: fetching them (with a valid
session) returns HTML with current values embedded in `value="..."` attributes
and/or `document.<form>.<field>.value="..."` assignments. Parse the HTML to read.

### Config SET (write values)
`POST` the form action with the same field names seen on the page. Each form
carries a hidden `submit-url` that the server redirects to on success.
The UI's "Save & Apply" button renames the hidden `save_apply_flag` (value `1`)
to `save_apply` before submitting — the CLI includes `save_apply=1` so the
change is committed rather than just previewed.

Key fields observed:

- **LAN** (`formTcpipSetup`): `lan_ip`, `lan_mask`, `lan_gateway`,
  `dhcpRangeStart`, `dhcpRangeEnd`, `dhcpLeaseTime`, `dns1`, `domainName`,
  `lan_macAddr`.
- **WiFi** (`formWlanSetup`): `ssid0` (SSID), `wlanDisabled0`, `channelbound0`,
  `wlanwmm0`, `wlanMacClone0`, plus a band selector (`wlan_id` / `wlanIntf`).
- **WAN** (`formWanConfigSetup`): WAN type (`dhcp`/`pppoe`/`static`/`bridge`),
  `pppIdleTime`, `wan_macAddr`, PPPoE/EAP fields.

## Notes / caveats
- Server returns `Connection: close` on responses; the CLI must reconnect per
  request (stateless over the cookie).
- Login uses MD5 (broken as a hash) — it is the device's own challenge scheme;
  the CLI must reproduce it, not use it for anything else.
