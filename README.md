# router-cli

Terminal control for the router web UI at `http://192.168.0.1` — reboot, send
USSD codes, read SMS messages, and read/edit WAN/LAN/WiFi config, without
clicking through the web UI.

Target device: an **Olax MC60** 4G LTE router (Realtek SDK, Boa web server),
but the commands are generic over the `--base-url` flag.

## Install / build

```sh
cargo build --release
# binary at target/release/router-cli
```

## Configure credentials

The admin password is never committed or logged. Provide it one of these ways
(highest precedence first):

1. `--password` command-line flag
2. `ROUTER_ADMIN_PASSWORD` environment variable
3. `~/.config/router-cli/config.toml`:

```toml
base_url = "http://192.168.0.1"
username = "admin"
password = "YOUR_PASSWORD"
```

Copy `config.example.toml` as a starting point. A missing password produces a
clear error telling you where to set it.

## Usage

```sh
# Reboot (waits for the device to go down and come back)
router-cli reboot

# Send a USSD code and print the carrier response
router-cli ussd '*100#'

# Read the last 5 received SMS messages (default: 10, newest first)
router-cli sms list -n 5

# Read them in full: one block per message, body wrapped to the terminal width
router-cli sms list -n 5 --full

# Machine-readable output for scripts
router-cli sms list -n 50 --json | jq -r '.[] | "\(.time) \(.number): \(.content)"'

# Read a config value
router-cli config get lan lan_ip
router-cli config get wifi ssid0
router-cli config get wan wanType

# Change a config value (re-submits the full form, then applies)
router-cli config set lan domainName myname.local
router-cli config set wifi ssid0 MyWiFi
```

Global options: `--base-url`, `--username`, `--password`.

### SMS

`router-cli sms list [-n N] [-f|--full] [--json]` reads the device inbox and
prints the newest `N` messages (default `10`) newest-first.

By default it prints a table of `TIME  FROM  STATUS  MESSAGE`, where `STATUS` is
`read` or `unread` and the body is squeezed onto one row.

`-f` / `--full` prints one block per message instead:

```text
[15,16,17] 2026-11-26 16:16:41 CAMTEL read
  Bonjour cher client. votre compte contient PrepaidBalanceSubaccount, le
  solde est de 102.75 FCFA, ...
```

The block body is the complete text the device stores, wrapped to the terminal
width (falls back to `COLUMNS`, then 100 columns when output is not a TTY).
Long words are split rather than dropped. `--full` and `--json` are mutually
exclusive.

`--json` emits an array of objects with `index`, `unread`, `number`, `time`, and
`content` fields; the content is always the full stored body (never wrapped).

This is read-only: fetching the inbox does not mark messages as read, and the
command cannot send or delete messages. Note that the router only stores ~160
characters per SMS part, so `content` (and `--full` output) can end mid-sentence
for messages the device itself truncated — that text is not available over the
web interface.

### Config sections

| Section | Page | Form action |
|---------|------|-------------|
| `wan`   | wanconfig.htm  | `/boafrm/formWanConfigSetup` |
| `lan`   | tcpiplan.htm   | `/boafrm/formTcpipSetup` |
| `wifi`  | wlbasic.htm    | `/boafrm/formWlanSetup` |

Use `router-cli config get <section> <key>` to read any field; the field names
are those shown in the page's HTML (`lan_ip`, `lan_mask`, `dhcpRangeStart`,
`dns1`, `domainName`, `ssid0`, `wanType`, …). `config get` on a known section
will tell you when a field name doesn't exist.

## How it works

The router authenticates with a challenge–response HMAC-MD5 login and tracks
the session with a `webuicookie` cookie. The CLI speaks a small tolerant
HTTP/1.1 client directly over TCP (the device's Boa server sends malformed
folded headers that strict HTTP clients reject), reproduces the HMAC-MD5 login,
then performs the same form POSTs the web UI uses. See
[`docs/router-api.md`](docs/router-api.md) for the full reverse-engineering
notes.

## Tests

```sh
cargo test
```

Unit tests cover the HMAC-MD5 login primitive (RFC 2202 vector), URL encoding,
HTML scraping helpers, and SMS inbox parsing/ordering/limiting.

Smoke test against a live router (needs credentials configured):

```sh
cargo run -- sms list -n 3
cargo run -- sms list -n 3 --json | jq .
```

## Security note

The device's login scheme uses MD5 because that is what the router itself
implements; the CLI does not use it for anything else. Never commit your real
password — `config.toml` is git-ignored.
